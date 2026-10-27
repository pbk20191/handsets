// Any other host (BSDs, …): softbuffer CPU blit of JPEG / tilejpeg frames.
// There is no H.264 decoder here, so GpuFrame has no values.

mod renderer;

use std::sync::Arc;

use winit::window::Window;

use crate::{DecodedFrame, Mapping};

pub enum GpuFrame {}

pub struct Renderer(renderer::SoftRenderer);

impl Renderer {
    pub const NAME: &'static str = "software";

    pub fn new(window: &Arc<Window>, _device: (u32, u32)) -> Option<Self> {
        renderer::SoftRenderer::new(window).map(Self)
    }

    /// The surface is resized on the next present.
    pub fn resized(&mut self, _w: u32, _h: u32) {}

    pub fn present(
        &mut self, frame: &DecodedFrame, window: (u32, u32), device: Option<(u32, u32)>,
    ) -> Option<Mapping> {
        match frame {
            DecodedFrame::Bgra { rgb, width, height } => {
                let device = device.unwrap_or((*width, *height));
                self.0.present(rgb, *width, *height, window.0, window.1, device)
            }
            DecodedFrame::Gpu(f) => match *f {},
        }
    }

    pub fn device_xy(
        &self, mapping: Option<Mapping>, _window: (u32, u32), x: f64, y: f64,
    ) -> Option<(i32, i32)> {
        mapping?.map(x, y)
    }
}

pub struct H264Decoder(std::convert::Infallible);

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        Err("h264 decode is only implemented on macOS and Windows".into())
    }

    pub fn decode(&mut self, _au: &[u8]) -> Result<Option<GpuFrame>, String> {
        match self.0 {}
    }
}
