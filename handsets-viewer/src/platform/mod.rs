// Per-platform presentation and H.264 decode. Each backend exports the same
// three names, picked at compile time:
//
//   Renderer    — draws the latest DecodedFrame into the window and maps
//                 cursor positions back to device pixels
//   H264Decoder — turns stream_h264 access units into GpuFrames
//   GpuFrame    — a decoded H.264 frame that stays on the GPU
//
// JpegDecoder decodes `stream` frames into GpuFrames in hardware; only macOS
// has one; elsewhere `new()` is None and JPEG goes through zune-jpeg.
//
//   macos/ — CAMetalLayer + VideoToolbox (IOSurface, zero-copy)
//   win/   — DXGI swap chain + D3D11 video processor + Media Foundation (DXVA)
//   linux/ — OpenGL ES 3 through EGL (Wayland / X11)
//   soft/  — softbuffer CPU blit for any other host; no H.264

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{GpuFrame, H264Decoder, JpegDecoder, Renderer};

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{GpuFrame, H264Decoder, Renderer};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{GpuFrame, H264Decoder, Renderer};

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod soft;
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub use soft::{GpuFrame, H264Decoder, Renderer};

#[cfg(not(target_os = "macos"))]
pub enum JpegDecoder {}

#[cfg(not(target_os = "macos"))]
impl JpegDecoder {
    pub fn new() -> Option<Self> {
        None
    }

    pub fn decode(&mut self, _jpeg: &[u8]) -> Result<GpuFrame, String> {
        match *self {}
    }
}
