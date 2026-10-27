// Windows presentation: a flip-model DXGI swap chain on the winit HWND, and
// one ID3D11VideoProcessor blit per frame that scales the frame into a
// letterboxed rectangle (and converts NV12 → RGB for decoded H.264). BGRA
// frames from the JPEG / tilejpeg streams are uploaded into a texture
// first; H.264 frames are decoder textures used in place.
use std::mem::ManuallyDrop;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0, D3D11_VIDEO_COLOR_RGBA,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIFactory2, IDXGISwapChain1, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use super::gpu::{gpu, Gpu};
use crate::Mapping;

/// The video processor is tied to input and output sizes; rebuilt when they change.
struct Processor {
    key: (u32, u32, u32, u32),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

pub struct D3dRenderer {
    gpu: &'static Gpu,
    swap: IDXGISwapChain1,
    size: (u32, u32),
    processor: Option<Processor>,
    /// BGRA upload target for CPU frames.
    upload: Option<(ID3D11Texture2D, u32, u32)>,
}

impl D3dRenderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Option<Self> {
        let gpu = gpu()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(1),
            Height: height.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: BOOL(0),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        let swap = unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
            factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).ok()?
        };
        Some(Self { gpu, swap, size: (width.max(1), height.max(1)), processor: None, upload: None })
    }

    /// Uploads a BGRA frame and presents it.
    pub fn present_bgra(
        &mut self, bgra: &[u8], w: u32, h: u32, win_w: u32, win_h: u32, device: (u32, u32),
    ) -> Option<Mapping> {
        if w == 0 || h == 0 || bgra.len() < (w * h * 4) as usize {
            return None;
        }
        let tex = self.upload_texture(w, h)?;
        unsafe {
            self.gpu.context.UpdateSubresource(&tex, 0, None, bgra.as_ptr().cast(), w * 4, 0);
        }
        self.present_texture(&tex, 0, w, h, win_w, win_h, device)
    }

    /// Presents `w`×`h` of array slice `slice` of `tex` (BGRA or NV12).
    #[allow(clippy::too_many_arguments)]
    pub fn present_texture(
        &mut self, tex: &ID3D11Texture2D, slice: u32, w: u32, h: u32, win_w: u32, win_h: u32,
        device: (u32, u32),
    ) -> Option<Mapping> {
        let (win_w, win_h) = (win_w.max(1), win_h.max(1));
        if self.size != (win_w, win_h) {
            unsafe {
                self.swap
                    .ResizeBuffers(0, win_w, win_h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
                    .ok()?;
            }
            self.size = (win_w, win_h);
        }

        let scale = f64::min(win_w as f64 / w as f64, win_h as f64 / h as f64);
        let out_w = ((w as f64 * scale) as u32).clamp(1, win_w);
        let out_h = ((h as f64 * scale) as u32).clamp(1, win_h);
        let pad_x = (win_w - out_w) / 2;
        let pad_y = (win_h - out_h) / 2;

        let gpu = self.gpu;
        let (enumerator, processor) = {
            let p = self.processor_for(w, h, win_w, win_h)?;
            (p.enumerator.clone(), p.processor.clone())
        };
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0).ok()?;
            let ov_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut ov: Option<ID3D11VideoProcessorOutputView> = None;
            gpu.video_device
                .CreateVideoProcessorOutputView(&back, &enumerator, &ov_desc, Some(&mut ov))
                .ok()?;
            let iv_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice },
                },
            };
            let mut iv: Option<ID3D11VideoProcessorInputView> = None;
            gpu.video_device
                .CreateVideoProcessorInputView(tex, &enumerator, &iv_desc, Some(&mut iv))
                .ok()?;

            let vc = &gpu.video_context;
            let src = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            let dst = RECT {
                left: pad_x as i32,
                top: pad_y as i32,
                right: (pad_x + out_w) as i32,
                bottom: (pad_y + out_h) as i32,
            };
            let full = RECT { left: 0, top: 0, right: win_w as i32, bottom: win_h as i32 };
            vc.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 },
                },
            };
            vc.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);

            let ov = ov?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(iv),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut streams = [stream];
            let blt = vc.VideoProcessorBlt(&processor, &ov, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            blt.ok()?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok().ok()?;
        }
        Some(Mapping { pad_x, pad_y, out_w, out_h, src_w: device.0, src_h: device.1 })
    }

    fn upload_texture(&mut self, w: u32, h: u32) -> Option<ID3D11Texture2D> {
        if let Some((t, tw, th)) = &self.upload {
            if (*tw, *th) == (w, h) {
                return Some(t.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()? };
        let tex = tex?;
        self.upload = Some((tex.clone(), w, h));
        Some(tex)
    }

    fn processor_for(&mut self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Option<&Processor> {
        let key = (in_w, in_h, out_w, out_h);
        if self.processor.as_ref().map(|p| p.key) != Some(key) {
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: in_w,
                InputHeight: in_h,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let vd = &self.gpu.video_device;
            let enumerator = unsafe { vd.CreateVideoProcessorEnumerator(&desc).ok()? };
            let processor = unsafe { vd.CreateVideoProcessor(&enumerator, 0).ok()? };
            self.processor = Some(Processor { key, enumerator, processor });
        }
        self.processor.as_ref()
    }
}

/// HWND of a winit window.
pub fn hwnd_of(window: &winit::window::Window) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}
