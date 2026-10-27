// Linux: OpenGL ES 3 through EGL on Wayland or X11.

mod gl;

use std::sync::Arc;

use winit::window::Window;

use crate::{DecodedFrame, Mapping};

/// No H.264 decoder on Linux yet, so there are no GPU frames.
pub enum GpuFrame {}

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
        Err("h264 decode is not implemented on Linux yet".into())
    }

    pub fn decode(&mut self, _au: &[u8]) -> Result<Option<GpuFrame>, String> {
        match self.0 {}
    }
}
