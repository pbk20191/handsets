// Per-platform presentation and H.264 decode. Each backend exports the same
// three names, picked at compile time:
//
//   Renderer    — draws the latest DecodedFrame into the window and maps
//                 cursor positions back to device pixels
//   H264Decoder — turns stream_h264 access units into GpuFrames
//   GpuFrame    — a decoded H.264 frame that stays on the GPU
//
// and two JPEG front ends:
//
//   JpegDecoder — one `stream` frame → DecodedFrame
//   TileCanvas  — `stream_tilejpeg` keyframes and tile patches → DecodedFrame
//
// macOS runs both on VideoToolbox + Metal; everywhere else they decode with
// zune-jpeg into a CPU frame (cpu_jpeg.rs).
//
//   macos/ — CAMetalLayer + VideoToolbox (IOSurface, zero-copy)
//   win/   — DXGI swap chain + D3D11 video processor + Media Foundation (DXVA)
//   linux/ — OpenGL ES 3 through EGL (Wayland / X11)
//   soft/  — softbuffer CPU blit for any other host; no H.264

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{GpuFrame, H264Decoder, JpegDecoder, Renderer, TileCanvas};

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
mod cpu_jpeg;
#[cfg(not(target_os = "macos"))]
pub use cpu_jpeg::{JpegDecoder, TileCanvas};

/// One changed tile of a tilejpeg delta, placed at (x, y) in the frame.
pub struct Tile<'a> {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub jpeg: &'a [u8],
}
