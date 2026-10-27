// macOS: CAMetalLayer presentation, VideoToolbox H.264 and JPEG (whole
// frames and tilejpeg tiles) straight into IOSurface-backed CVPixelBuffers
// that Metal blits without a copy. Nothing is decoded on the CPU.

mod metal;
mod tiles;
mod videotoolbox;

use std::sync::Arc;

use winit::window::Window;

use crate::{DecodedFrame, Mapping};

pub use tiles::TileCanvas;
pub use videotoolbox::PixelBuffer as GpuFrame;

pub struct Renderer(metal::MetalRenderer);

impl Renderer {
    pub const NAME: &'static str = "Metal";

    pub fn new(window: &Arc<Window>, device: (u32, u32)) -> Option<Self> {
        metal::MetalRenderer::new(window, device.0, device.1).map(Self)
    }

    pub fn resized(&mut self, w: u32, h: u32) {
        self.0.resize_window(w, h);
    }

    /// The Metal layer letterboxes on its own, so there is no Mapping to
    /// return; [Self::device_xy] asks the renderer instead.
    pub fn present(
        &mut self, frame: &DecodedFrame, _window: (u32, u32), _device: Option<(u32, u32)>,
    ) -> Option<Mapping> {
        let DecodedFrame::Gpu(pb) = frame;
        self.0.present_pixel_buffer(pb);
        None
    }

    pub fn device_xy(
        &self, _mapping: Option<Mapping>, window: (u32, u32), x: f64, y: f64,
    ) -> Option<(i32, i32)> {
        self.0.window_pos_to_pixel(window.0.max(1), window.1.max(1), x, y)
    }
}

pub struct H264Decoder(videotoolbox::VtDecoder);

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self(videotoolbox::VtDecoder::new()))
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Option<GpuFrame>, String> {
        self.0.decode(au)
    }
}

pub struct JpegDecoder(videotoolbox::VtJpegDecoder);

impl JpegDecoder {
    pub const LABEL: &'static str = "jpeg-vt";

    pub fn new() -> Self {
        Self(videotoolbox::VtJpegDecoder::new())
    }

    pub fn decode(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        self.0.decode(jpeg).map(DecodedFrame::Gpu)
    }
}
