// JPEG and tilejpeg for hosts without a GPU JPEG path: zune-jpeg decodes
// into CPU BGRA buffers that the platform renderer uploads.

use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use super::Tile;
use crate::DecodedFrame;

/// Decodes into a recycled buffer: a 1080x2400 frame is 10 MB, and
/// allocating and zero-filling one per frame costs about as much CPU as a
/// third of the decode.
pub struct JpegDecoder {
    spare: Option<Vec<u8>>,
}

impl JpegDecoder {
    pub const LABEL: &'static str = "jpeg";

    pub fn new() -> Self {
        Self { spare: None }
    }

    pub fn decode(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        let mut buf = self.spare.take().unwrap_or_default();
        let (w, h) = decode_bgra_into(jpeg, &mut buf)?;
        Ok(DecodedFrame::Bgra { rgb: buf, width: w, height: h })
    }

    /// Takes back a frame the UI thread no longer shows, for its buffer.
    pub fn recycle(&mut self, old: DecodedFrame) {
        if let DecodedFrame::Bgra { rgb, .. } = old {
            self.spare = Some(rgb);
        }
    }
}

/// Persistent BGRA framebuffer, replaced by keyframes and patched by tiles.
/// Each published frame is a copy (the UI thread reads it while the next
/// packet patches `fb`), made into a recycled buffer.
pub struct TileCanvas {
    fb: Vec<u8>,
    w: u32,
    h: u32,
    spare: Option<Vec<u8>>,
}

impl TileCanvas {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        Ok(Self { fb: vec![0u8; w as usize * h as usize * 4], w, h, spare: None })
    }

    pub fn keyframe(&mut self, jpeg: &[u8]) -> Result<DecodedFrame, String> {
        let mut buf = self.spare.take().unwrap_or_default();
        let (w, h) = decode_bgra_into(jpeg, &mut buf)?;
        if (w, h) == (self.w, self.h) {
            // The decoded keyframe is the new framebuffer; publish a copy.
            std::mem::swap(&mut self.fb, &mut buf);
            self.spare = Some(buf);
        } else {
            // Source dimensions don't match — letterbox into fb.
            blit_into(&mut self.fb, self.w as usize, 0, 0, w as usize, h as usize, &buf);
            self.spare = Some(buf);
        }
        Ok(self.frame())
    }

    pub fn patch(&mut self, tiles: &[Tile]) -> Result<DecodedFrame, String> {
        let mut tile = Vec::new();
        for t in tiles {
            let Ok((w, h)) = decode_bgra_into(t.jpeg, &mut tile) else {
                continue;
            };
            if (w, h) != (t.w, t.h) {
                eprintln!("tilejpeg: tile size mismatch (jpeg {w}x{h}, hdr {}x{})", t.w, t.h);
                continue;
            }
            blit_into(&mut self.fb, self.w as usize, t.x as usize, t.y as usize, w as usize, h as usize, &tile);
        }
        Ok(self.frame())
    }

    /// Takes back a frame the UI thread no longer shows, for its buffer.
    pub fn recycle(&mut self, old: DecodedFrame) {
        if let DecodedFrame::Bgra { rgb, .. } = old {
            self.spare = Some(rgb);
        }
    }

    fn frame(&mut self) -> DecodedFrame {
        let mut out = self.spare.take().unwrap_or_default();
        out.clear();
        out.extend_from_slice(&self.fb);
        DecodedFrame::Bgra { rgb: out, width: self.w, height: self.h }
    }
}
/// Decodes a JPEG to BGRA8888 with zune-jpeg into `out`, resized to fit;
/// a buffer that already has the size is reused without being cleared.
fn decode_bgra_into(jpeg: &[u8], out: &mut Vec<u8>) -> Result<(u32, u32), String> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    decoder.decode_headers().map_err(|e| format!("jpeg decode error: {e:?}"))?;
    let info = decoder.info().ok_or("jpeg decode error: no header")?;
    let n = decoder.output_buffer_size().ok_or("jpeg decode error: no output size")?;
    out.resize(n, 0);
    decoder.decode_into(out).map_err(|e| format!("jpeg decode error: {e:?}"))?;
    Ok((info.width as u32, info.height as u32))
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
