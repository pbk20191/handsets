// Linux: OpenGL ES 3 through EGL on Wayland or X11, and libavcodec + VA-API
// H.264 whose DRM PRIME output is imported into GL as dma-buf EGLImages.

mod gl;
mod vaapi;

use std::sync::Arc;

use winit::window::Window;

use crate::{DecodedFrame, Mapping};

pub use vaapi::GpuFrame;

pub struct Renderer(gl::GlRenderer);

impl Renderer {
    pub const NAME: &'static str = "OpenGL ES (EGL)";

    pub fn new(window: &Arc<Window>, _device: (u32, u32)) -> Option<Self> {
        match gl::GlRenderer::new(window) {
            Ok(r) => Some(Self(r)),
            Err(e) => {
                eprintln!("viewer: {e}");
                None
            }
        }
    }

    /// The EGL surface is resized on the next present.
    pub fn resized(&mut self, _w: u32, _h: u32) {}

    pub fn present(
        &mut self, frame: &DecodedFrame, window: (u32, u32), device: Option<(u32, u32)>,
    ) -> Option<Mapping> {
        match frame {
            DecodedFrame::Bgra { rgb, width, height } => {
                let device = device.unwrap_or((*width, *height));
                self.0.present_bgra(rgb, *width, *height, window.0, window.1, device)
            }
            DecodedFrame::Gpu(f) => {
                let device = device.unwrap_or((f.width, f.height));
                self.0.present_dmabuf(f, window.0, window.1, device)
            }
        }
    }

    pub fn device_xy(
        &self, mapping: Option<Mapping>, _window: (u32, u32), x: f64, y: f64,
    ) -> Option<(i32, i32)> {
        mapping?.map(x, y)
    }
}

pub struct H264Decoder(vaapi::VaapiDecoder);

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        vaapi::VaapiDecoder::new().map(Self).map_err(|e| format!("h264 decoder: {e}"))
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Option<GpuFrame>, String> {
        self.0.decode(au)
    }
}
