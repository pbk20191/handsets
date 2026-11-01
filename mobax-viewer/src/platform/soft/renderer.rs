// CPU presentation path for hosts without a GPU backend: letterbox-fits
// the latest BGRA frame into the window with nearest-neighbour scaling and
// hands it to softbuffer. Good enough for the JPEG / tilejpeg streams;
// macOS, Windows and Linux use their GPU backends.
use std::num::NonZeroU32;
use std::sync::Arc;

use winit::window::Window;

use crate::Mapping;

pub struct SoftRenderer {
    // The context must outlive the surface.
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// Source byte offset for each output column, rebuilt when the fit changes.
    xmap: Vec<usize>,
    xmap_key: (u32, u32),
}

impl SoftRenderer {
    pub fn new(window: &Arc<Window>) -> Option<Self> {
        let context = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
        Some(Self { _context: context, surface, xmap: Vec::new(), xmap_key: (0, 0) })
    }

    /// Draws `bgra` (`w`×`h`, 4 bytes per pixel) centred in a `win_w`×`win_h`
    /// window, black bars around it. Returns the window→device mapping for
    /// input, where `device` is the device's real resolution.
    pub fn present(
        &mut self,
        bgra: &[u8],
        w: u32,
        h: u32,
        win_w: u32,
        win_h: u32,
        device: (u32, u32),
    ) -> Option<Mapping> {
        let stride = w as usize * 4;
        if w == 0 || h == 0 || bgra.len() < stride * h as usize {
            return None;
        }
        let (ww, wh) = (NonZeroU32::new(win_w)?, NonZeroU32::new(win_h)?);
        self.surface.resize(ww, wh).ok()?;

        let scale = f64::min(win_w as f64 / w as f64, win_h as f64 / h as f64);
        let out_w = ((w as f64 * scale) as u32).clamp(1, win_w);
        let out_h = ((h as f64 * scale) as u32).clamp(1, win_h);
        let pad_x = (win_w - out_w) / 2;
        let pad_y = (win_h - out_h) / 2;

        if self.xmap_key != (w, out_w) {
            self.xmap.clear();
            self.xmap
                .extend((0..out_w).map(|x| (x as u64 * w as u64 / out_w as u64) as usize * 4));
            self.xmap_key = (w, out_w);
        }

        let mut buf = self.surface.buffer_mut().ok()?;
        buf.fill(0);
        let row_len = win_w as usize;
        for oy in 0..out_h {
            let sy = (oy as u64 * h as u64 / out_h as u64) as usize;
            let src = &bgra[sy * stride..(sy + 1) * stride];
            let start = (pad_y + oy) as usize * row_len + pad_x as usize;
            let dst = &mut buf[start..start + out_w as usize];
            for (d, &i) in dst.iter_mut().zip(&self.xmap) {
                // softbuffer pixels are 0x00RRGGBB.
                *d = (src[i + 2] as u32) << 16 | (src[i + 1] as u32) << 8 | src[i] as u32;
            }
        }
        buf.present().ok()?;

        Some(Mapping { pad_x, pad_y, out_w, out_h, src_w: device.0, src_h: device.1 })
    }
}
