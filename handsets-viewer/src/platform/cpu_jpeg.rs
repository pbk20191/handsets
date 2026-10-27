// JPEG and tilejpeg for hosts without a GPU JPEG path: zune-jpeg decodes
// into CPU BGRA buffers that the platform renderer uploads.

use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use super::Tile;
use crate::DecodedFrame;

pub struct JpegDecoder;

impl JpegDecoder {
    pub const LABEL: &'static str = "jpeg";

    pub fn new() -> Self {
        Self
    }

    pub fn decode(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        let (w, h, bgra) = decode_bgra(jpeg)?;
        Ok(DecodedFrame::Bgra { rgb: bgra, width: w as u32, height: h as u32 })
    }
}

/// Persistent BGRA framebuffer, replaced by keyframes and patched by tiles.
pub struct TileCanvas {
    fb: Vec<u8>,
    w: u32,
    h: u32,
}

impl TileCanvas {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        Ok(Self { fb: vec![0u8; w as usize * h as usize * 4], w, h })
    }

    pub fn keyframe(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        let (w, h, bgra) = decode_bgra(jpeg)?;
        if (w as u32, h as u32) == (self.w, self.h) && bgra.len() == self.fb.len() {
            self.fb.copy_from_slice(&bgra);
        } else {
            // Source dimensions don't match — letterbox into fb.
            blit_into(&mut self.fb, self.w as usize, 0, 0, w, h, &bgra);
        }
        Ok(self.frame())
    }

    pub fn patch(&mut self, tiles: &[Tile]) -> Result<DecodedFrame, String> {
        for t in tiles {
            let Ok((w, h, bgra)) = decode_bgra(t.jpeg) else {
                continue;
            };
            if (w as u32, h as u32) != (t.w, t.h) {
                eprintln!("tilejpeg: tile size mismatch (jpeg {w}x{h}, hdr {}x{})", t.w, t.h);
                continue;
            }
            blit_into(&mut self.fb, self.w as usize, t.x as usize, t.y as usize, w, h, &bgra);
        }
        Ok(self.frame())
    }

    fn frame(&self) -> DecodedFrame {
        DecodedFrame::Bgra { rgb: self.fb.clone(), width: self.w, height: self.h }
    }
}

/// Decode a JPEG byte slice to BGRA8888 using zune-jpeg.
fn decode_bgra(jpeg: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(jpeg, opts);
    let bgra = decoder.decode().map_err(|e| format!("jpeg decode error: {e:?}"))?;
    let info = decoder.info().ok_or("jpeg decode error: no header")?;
    Ok((info.width as usize, info.height as usize, bgra))
}

/// Copy a tile from `src` (BGRA8888, tw*th*4 bytes) into `fb` at offset (dx, dy).
/// `fb_w` is the row stride in pixels of the destination framebuffer.
fn blit_into(
    fb: &mut [u8], fb_w: usize, dx: usize, dy: usize,
    tw: usize, th: usize, src: &[u8],
) {
    if tw == 0 || th == 0 { return; }
    let row_bytes = tw * 4;
    for y in 0..th {
        let src_off = y * row_bytes;
        let dst_off = ((dy + y) * fb_w + dx) * 4;
        if dst_off + row_bytes > fb.len() { break; }
        if src_off + row_bytes > src.len() { break; }
        fb[dst_off..dst_off + row_bytes]
            .copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
}
