// Per-platform presentation and H.264 decode. Each backend exports the same
// three names, picked at compile time:
//
//   Renderer    — draws the latest DecodedFrame into the window and maps
//                 cursor positions back to device pixels
//   H264Decoder — turns stream_h264 access units into GpuFrames
//   GpuFrame    — a decoded H.264 frame that stays on the GPU
//
//   macos/ — CAMetalLayer + VideoToolbox (IOSurface, zero-copy)
//   win/   — DXGI swap chain + D3D11 video processor + Media Foundation (DXVA)
//   soft/  — softbuffer CPU blit; no H.264

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{GpuFrame, H264Decoder, Renderer};

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{GpuFrame, H264Decoder, Renderer};

#[cfg(not(any(target_os = "macos", windows)))]
mod soft;
#[cfg(not(any(target_os = "macos", windows)))]
pub use soft::{GpuFrame, H264Decoder, Renderer};
