// OpenGL ES 3 presentation through EGL (Wayland or X11): an EGL window
// surface on the winit window, one textured full-viewport triangle per frame
// drawn into a letterboxed glViewport. BGRA frames are uploaded as RGBA8 and
// swizzled in the shader.

use std::num::NonZeroU32;
use std::sync::Arc;

use glow::HasContext;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, Version,
};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

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

pub struct GlRenderer {
    gl: glow::Context,
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    size: (u32, u32),
    bgra_program: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
    tex_size: (u32, u32),
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
            let vao = gl.create_vertex_array()?;
            let texture = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (k, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
            }
            Ok(Self {
                gl,
                context,
                surface,
                size: (w, h),
                bgra_program,
                vao,
                texture,
                tex_size: (0, 0),
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
