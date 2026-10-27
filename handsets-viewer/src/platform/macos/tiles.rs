// tilejpeg on the GPU: VideoToolbox decodes the keyframes and tiles, and a
// Metal blit pass builds each new frame in a fresh pool buffer — the previous
// frame first, then the changed tiles on top. Taking a new buffer per frame
// means the renderer can still be reading the last one.

use std::ptr::{self, NonNull};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin,
};

use super::metal::{origin, size, texture_for};
use super::videotoolbox::{bgra_attributes, PixelBuffer, VtJpegDecoder};
use crate::platform::Tile;
use crate::DecodedFrame;

pub struct TileCanvas {
    dec: VtJpegDecoder,
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pool: CFRetained<CVPixelBufferPool>,
    size: (u32, u32),
    current: Option<PixelBuffer>,
}

// The canvas lives on the tilejpeg network thread only.
unsafe impl Send for TileCanvas {}

impl TileCanvas {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        let device = MTLCreateSystemDefaultDevice().ok_or("no Metal device")?;
        let queue = device.newCommandQueue().ok_or("no Metal command queue")?;
        let attrs = bgra_attributes(Some((w, h)));
        let mut pool: *mut CVPixelBufferPool = ptr::null_mut();
        let s = unsafe {
            CVPixelBufferPool::create(None, None, Some(attrs.as_opaque()), NonNull::from(&mut pool))
        };
        let pool = NonNull::new(pool)
            .filter(|_| s == 0)
            .ok_or_else(|| format!("CVPixelBufferPoolCreate: {s}"))?;
        Ok(Self {
            dec: VtJpegDecoder::new(),
            device,
            queue,
            // SAFETY: Create returns +1.
            pool: unsafe { CFRetained::from_raw(pool) },
            size: (w, h),
            current: None,
        })
    }

    /// A full-frame JPEG. At canvas size it becomes the frame as is; any
    /// other size is drawn at the top-left over the previous frame.
    pub fn keyframe(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        let pb = self.dec.decode(jpeg)?;
        if (pb.width(), pb.height()) == self.size {
            self.current = Some(pb.clone());
            return Ok(DecodedFrame::Gpu(pb));
        }
        self.compose(&[(0, 0, pb)])
    }

    /// Changed tiles, drawn over the previous frame.
    /// Frames come from a CVPixelBuffer pool; there is nothing to reuse.
    pub fn recycle(&mut self, _old: DecodedFrame) {}

    pub fn patch(&mut self, tiles: &[Tile]) -> Result<DecodedFrame, String> {
        let mut decoded = Vec::with_capacity(tiles.len());
        for t in tiles {
            let pb = self.dec.decode(t.jpeg)?;
            if (pb.width(), pb.height()) != (t.w, t.h) {
                eprintln!(
                    "tilejpeg: tile size mismatch (jpeg {}x{}, hdr {}x{})",
                    pb.width(), pb.height(), t.w, t.h
                );
                continue;
            }
            decoded.push((t.x, t.y, pb));
        }
        self.compose(&decoded)
    }

    fn compose(&mut self, items: &[(u32, u32, PixelBuffer)]) -> Result<DecodedFrame, String> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        let s = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut raw)) };
        let target = NonNull::new(raw)
            .filter(|_| s == 0)
            .ok_or_else(|| format!("CVPixelBufferPoolCreatePixelBuffer: {s}"))?;
        // SAFETY: Create returns +1.
        let target = PixelBuffer(unsafe { CFRetained::from_raw(target) });
        let dst = texture_for(&self.device, &target).ok_or("pool buffer has no IOSurface")?;

        let cmd = self.queue.commandBuffer().ok_or("no Metal command buffer")?;
        let blit = cmd.blitCommandEncoder().ok_or("no Metal blit encoder")?;
        let (cw, ch) = self.size;
        let copy = |src: &PixelBuffer, x: u32, y: u32| {
            if x >= cw || y >= ch {
                return;
            }
            let Some(tex) = texture_for(&self.device, src) else {
                return;
            };
            let w = src.width().min(cw - x);
            let h = src.height().min(ch - y);
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    &tex, 0, 0, origin(), size(w, h),
                    &dst, 0, 0, MTLOrigin { x: x as usize, y: y as usize, z: 0 },
                );
            }
        };
        if let Some(prev) = &self.current {
            copy(prev, 0, 0);
        }
        for (x, y, pb) in items {
            copy(pb, *x, *y);
        }
        blit.endEncoding();
        cmd.commit();
        cmd.waitUntilCompleted();

        self.current = Some(target.clone());
        Ok(DecodedFrame::Gpu(target))
    }
}
