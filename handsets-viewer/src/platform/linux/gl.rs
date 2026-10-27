// OpenGL ES 3 presentation through EGL (Wayland or X11): an EGL window
// surface on the winit window, one textured full-viewport triangle per frame
// drawn into a letterboxed glViewport. BGRA frames are uploaded as RGBA8 and
// swizzled in the shader; decoded H.264 frames arrive as DRM PRIME dma-bufs,
// imported per plane as EGLImages (Y → R8, CbCr → GR88) and converted to RGB
// in the shader.

use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;
use std::sync::Arc;

use glow::HasContext;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, Version,
};
use glutin::display::{AsRawDisplay, Display, DisplayApiPreference, GlDisplay, RawDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use super::vaapi::GpuFrame;
use crate::Mapping;

const VERTEX: &str = r#"#version 300 es
out vec2 uv;
void main() {
    // One triangle that covers the viewport; uv is flipped so row 0 is on top.
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = vec2(p.x, 1.0 - p.y);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT_BGRA: &str = r#"#version 300 es
precision mediump float;
in vec2 uv;
uniform sampler2D tex;
out vec4 color;
void main() {
    // Bytes arrive as B,G,R,A in the R,G,B,A channels.
    color = vec4(texture(tex, uv).bgr, 1.0);
}
"#;

const FRAGMENT_NV12: &str = r#"#version 300 es
precision mediump float;
in vec2 uv;
uniform sampler2D luma;
uniform sampler2D chroma;
out vec4 color;
void main() {
    // BT.601, limited range — what Android's AVC encoder emits.
    float y = (texture(luma, uv).r - 0.0625) * 1.1644;
    vec2 c = texture(chroma, uv).rg - vec2(0.5);
    color = vec4(y + 1.5960 * c.y, y - 0.3918 * c.x - 0.8130 * c.y, y + 2.0172 * c.x, 1.0);
}
"#;

// EGL_EXT_image_dma_buf_import(_modifiers) and DRM fourccs.
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;
const EGL_NONE: i32 = 0x3038;
const EGL_EXTENSIONS: i32 = 0x3055;
const DRM_FORMAT_R8: u32 = 0x2020_3852;
const DRM_FORMAT_GR88: u32 = 0x3838_5247;
const DRM_FORMAT_NV12: u32 = 0x3231_564E;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

type EglCreateImage =
    unsafe extern "C" fn(*const c_void, *const c_void, u32, *const c_void, *const i32) -> *const c_void;
type EglDestroyImage = unsafe extern "C" fn(*const c_void, *const c_void) -> u32;
type EglQueryString = unsafe extern "C" fn(*const c_void, i32) -> *const std::ffi::c_char;
type GlImageTargetTexture = unsafe extern "C" fn(u32, *const c_void);

/// dma-buf import entry points; None if EGL lacks the extension.
struct DmaBufImport {
    display: *const c_void,
    create: EglCreateImage,
    destroy: EglDestroyImage,
    target: GlImageTargetTexture,
    modifiers: bool,
}

/// One plane to import: dma-buf fd, layout, format and size.
struct Plane {
    fd: i32,
    offset: i32,
    pitch: i32,
    modifier: u64,
    fourcc: u32,
    width: u32,
    height: u32,
}

pub struct GlRenderer {
    gl: glow::Context,
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    size: (u32, u32),
    bgra_program: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
    tex_size: (u32, u32),
    nv12_program: glow::Program,
    luma: glow::Texture,
    chroma: glow::Texture,
    dmabuf: Option<DmaBufImport>,
    // Keeps the window (and so the native surface) alive as long as the EGL surface.
    _window: Arc<Window>,
}

impl GlRenderer {
    pub fn new(window: &Arc<Window>) -> Result<Self, String> {
        let raw_display = window.display_handle().map_err(|e| e.to_string())?.as_raw();
        let raw_window = window.window_handle().map_err(|e| e.to_string())?.as_raw();
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        unsafe {
            let display = Display::new(raw_display, DisplayApiPreference::Egl)
                .map_err(|e| format!("EGL display: {e}"))?;
            let template = ConfigTemplateBuilder::new().compatible_with_native_window(raw_window).build();
            let config = display
                .find_configs(template)
                .map_err(|e| format!("EGL config: {e}"))?
                .max_by_key(|c| c.num_samples() == 0)
                .ok_or("no EGL config")?;
            let attrs = ContextAttributesBuilder::new()
                .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
                .build(Some(raw_window));
            let not_current = display
                .create_context(&config, &attrs)
                .map_err(|e| format!("GLES 3 context: {e}"))?;
            let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
                raw_window,
                NonZeroU32::new(w).unwrap(),
                NonZeroU32::new(h).unwrap(),
            );
            let surface = display
                .create_window_surface(&config, &surface_attrs)
                .map_err(|e| format!("EGL window surface: {e}"))?;
            let context = not_current.make_current(&surface).map_err(|e| e.to_string())?;
            let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::new(1).unwrap()));

            let gl = glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s));
            let bgra_program = program(&gl, VERTEX, FRAGMENT_BGRA)?;
            let nv12_program = program(&gl, VERTEX, FRAGMENT_NV12)?;
            gl.use_program(Some(nv12_program));
            gl.uniform_1_i32(gl.get_uniform_location(nv12_program, "luma").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(nv12_program, "chroma").as_ref(), 1);
            let vao = gl.create_vertex_array()?;
            let texture = linear_texture(&gl)?;
            let luma = linear_texture(&gl)?;
            let chroma = linear_texture(&gl)?;
            let dmabuf = dmabuf_import(&display);
            Ok(Self {
                gl,
                context,
                surface,
                size: (w, h),
                bgra_program,
                vao,
                texture,
                tex_size: (0, 0),
                nv12_program,
                luma,
                chroma,
                dmabuf,
                _window: window.clone(),
            })
        }
    }

    pub fn present_bgra(
        &mut self, bgra: &[u8], w: u32, h: u32, win_w: u32, win_h: u32, device: (u32, u32),
    ) -> Option<Mapping> {
        if w == 0 || h == 0 || bgra.len() < (w * h * 4) as usize {
            return None;
        }
        let gl = &self.gl;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            let pixels = glow::PixelUnpackData::Slice(Some(&bgra[..(w * h * 4) as usize]));
            if self.tex_size == (w, h) {
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, pixels);
            } else {
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, pixels);
                self.tex_size = (w, h);
            }
        }
        let (program, texture) = (self.bgra_program, self.texture);
        self.draw(program, w, h, win_w, win_h, device, move |gl| unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        })
    }

    /// Imports the frame's two planes as EGLImages and draws them through the
    /// NV12 shader.
    pub fn present_dmabuf(
        &mut self, frame: &GpuFrame, win_w: u32, win_h: u32, device: (u32, u32),
    ) -> Option<Mapping> {
        let Some(import) = self.dmabuf.as_ref() else {
            eprintln!("viewer: EGL lacks EGL_EXT_image_dma_buf_import; cannot show H.264");
            return None;
        };
        let (y, uv) = planes(frame)?;
        let images = [import.image(&y)?, import.image(&uv)?];
        let (luma, chroma, program) = (self.luma, self.chroma, self.nv12_program);
        let target = import.target;
        let m = self.draw(program, frame.width, frame.height, win_w, win_h, device, move |gl| unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(luma));
            target(glow::TEXTURE_2D, images[0]);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(chroma));
            target(glow::TEXTURE_2D, images[1]);
        });
        if let Some(import) = self.dmabuf.as_ref() {
            for img in images {
                unsafe { (import.destroy)(import.display, img) };
            }
        }
        m
    }

    /// Clears to black, draws `program` into the letterboxed viewport for a
    /// `w`×`h` frame, and swaps.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self, program: glow::Program, w: u32, h: u32, win_w: u32, win_h: u32,
        device: (u32, u32), bind: impl FnOnce(&glow::Context),
    ) -> Option<Mapping> {
        let (win_w, win_h) = (win_w.max(1), win_h.max(1));
        if self.size != (win_w, win_h) {
            self.surface.resize(&self.context, NonZeroU32::new(win_w)?, NonZeroU32::new(win_h)?);
            self.size = (win_w, win_h);
        }
        let scale = f64::min(win_w as f64 / w as f64, win_h as f64 / h as f64);
        let out_w = ((w as f64 * scale) as u32).clamp(1, win_w);
        let out_h = ((h as f64 * scale) as u32).clamp(1, win_h);
        let pad_x = (win_w - out_w) / 2;
        let pad_y = (win_h - out_h) / 2;

        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, win_w as i32, win_h as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            // GL's origin is bottom-left.
            gl.viewport(pad_x as i32, (win_h - pad_y - out_h) as i32, out_w as i32, out_h as i32);
            gl.use_program(Some(program));
            bind(gl);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        self.surface.swap_buffers(&self.context).ok()?;
        Some(Mapping { pad_x, pad_y, out_w, out_h, src_w: device.0, src_h: device.1 })
    }
}

unsafe fn program(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    let p = gl.create_program()?;
    let mut shaders = Vec::new();
    for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
        let s = gl.create_shader(kind)?;
        gl.shader_source(s, src);
        gl.compile_shader(s);
        if !gl.get_shader_compile_status(s) {
            return Err(format!("shader: {}", gl.get_shader_info_log(s)));
        }
        gl.attach_shader(p, s);
        shaders.push(s);
    }
    gl.link_program(p);
    if !gl.get_program_link_status(p) {
        return Err(format!("program: {}", gl.get_program_info_log(p)));
    }
    for s in shaders {
        gl.detach_shader(p, s);
        gl.delete_shader(s);
    }
    Ok(p)
}

unsafe fn linear_texture(gl: &glow::Context) -> Result<glow::Texture, String> {
    let t = gl.create_texture()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(t));
    for (k, v) in [
        (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
        (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
        (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
    ] {
        gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
    }
    Ok(t)
}

unsafe fn dmabuf_import(display: &Display) -> Option<DmaBufImport> {
    let RawDisplay::Egl(egl) = display.raw_display();
    let sym = |name: &CStr| {
        let p = display.get_proc_address(name);
        (!p.is_null()).then_some(p)
    };
    let query: EglQueryString = std::mem::transmute(sym(c"eglQueryString")?);
    let ext = query(egl, EGL_EXTENSIONS);
    let ext = if ext.is_null() { "" } else { CStr::from_ptr(ext).to_str().unwrap_or("") };
    if !ext.contains("EGL_EXT_image_dma_buf_import") {
        return None;
    }
    Some(DmaBufImport {
        display: egl,
        create: std::mem::transmute(sym(c"eglCreateImageKHR")?),
        destroy: std::mem::transmute(sym(c"eglDestroyImageKHR")?),
        target: std::mem::transmute(sym(c"glEGLImageTargetTexture2DOES")?),
        modifiers: ext.contains("EGL_EXT_image_dma_buf_import_modifiers"),
    })
}

impl DmaBufImport {
    fn image(&self, p: &Plane) -> Option<*const c_void> {
        let mut attrs = vec![
            EGL_WIDTH, p.width as i32,
            EGL_HEIGHT, p.height as i32,
            EGL_LINUX_DRM_FOURCC_EXT, p.fourcc as i32,
            EGL_DMA_BUF_PLANE0_FD_EXT, p.fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT, p.offset,
            EGL_DMA_BUF_PLANE0_PITCH_EXT, p.pitch,
        ];
        if self.modifiers && p.modifier != DRM_FORMAT_MOD_INVALID {
            attrs.extend([
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, p.modifier as u32 as i32,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (p.modifier >> 32) as u32 as i32,
            ]);
        }
        attrs.push(EGL_NONE);
        let img = unsafe {
            (self.create)(self.display, std::ptr::null(), EGL_LINUX_DMA_BUF_EXT, std::ptr::null(), attrs.as_ptr())
        };
        if img.is_null() {
            eprintln!("viewer: eglCreateImage(dma-buf) failed for fourcc {:#x}", p.fourcc);
            return None;
        }
        Some(img)
    }
}

/// The luma and chroma planes of an NV12 frame, whether the driver exported
/// one NV12 layer with two planes or separate R8 and GR88 layers.
fn planes(frame: &GpuFrame) -> Option<(Plane, Plane)> {
    let d = frame.descriptor();
    let (w, h) = (frame.width, frame.height);
    let plane = |layer: usize, idx: usize, fourcc: u32, pw: u32, ph: u32| {
        let pl = &d.layers[layer].planes[idx];
        let obj = &d.objects[pl.object_index as usize];
        Plane {
            fd: obj.fd,
            offset: pl.offset as i32,
            pitch: pl.pitch as i32,
            modifier: obj.format_modifier,
            fourcc,
            width: pw,
            height: ph,
        }
    };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if d.nb_layers == 1 && d.layers[0].format == DRM_FORMAT_NV12 && d.layers[0].nb_planes >= 2 {
        Some((plane(0, 0, DRM_FORMAT_R8, w, h), plane(0, 1, DRM_FORMAT_GR88, cw, ch)))
    } else if d.nb_layers >= 2 {
        Some((plane(0, 0, d.layers[0].format, w, h), plane(1, 0, d.layers[1].format, cw, ch)))
    } else {
        eprintln!("viewer: unsupported DRM PRIME layout ({} layers, fourcc {:#x})", d.nb_layers, d.layers[0].format);
        None
    }
}
