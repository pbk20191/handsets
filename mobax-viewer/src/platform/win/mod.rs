// Windows: a DXGI flip swap chain on the winit HWND with one D3D11
// video-processor blit per frame, and Media Foundation H.264 decoding on the
// same D3D11 device (DXVA), so NV12 surfaces never leave the GPU.

mod d3d;
mod gpu;
mod media_foundation;

use std::sync::Arc;

use winit::window::Window;

use crate::{DecodedFrame, Mapping};

pub use media_foundation::GpuFrame;

pub struct Renderer(d3d::D3dRenderer);

impl Renderer {
    pub const NAME: &'static str = "D3D11";

    pub fn new(window: &Arc<Window>, _device: (u32, u32)) -> Option<Self> {
        let size = window.inner_size();
        d3d::D3dRenderer::new(d3d::hwnd_of(window)?, size.width, size.height).map(Self)
    }

    /// The swap chain is resized on the next present.
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
                self.0.present_texture(&f.texture, f.slice, f.width, f.height, window.0, window.1, device)
            }
        }
    }

    pub fn device_xy(
        &self, mapping: Option<Mapping>, _window: (u32, u32), x: f64, y: f64,
    ) -> Option<(i32, i32)> {
        mapping?.map(x, y)
    }
}

pub struct H264Decoder(media_foundation::MfDecoder);

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        let gpu = gpu::gpu().ok_or("h264 needs a D3D11 device")?;
        media_foundation::MfDecoder::new(gpu).map(Self).map_err(|e| format!("h264 decoder: {e}"))
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Option<GpuFrame>, String> {
        self.0.decode(au).map_err(|e| e.to_string())
    }
}
