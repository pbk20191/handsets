// macOS-only Metal-backed renderer.
//
// Skips `softbuffer`'s `CALayer` + Core Animation compositor path, which on
// macOS adds ~16–32 ms of WindowServer batching latency per frame. We attach
// a `CAMetalLayer` directly to the window's NSView and blit each decoded
// IOSurface into the drawable's texture, then present. Every codec decodes
// on VideoToolbox, so no frame is ever uploaded from the CPU.
//
// Drawable size is fixed at the device's native pixel dimensions; the layer's
// view bounds (set by NSView resize) handle visual letterboxing automatically
// via the contentsGravity = resizeAspect default.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::msg_send;
use objc2_core_foundation::CGSize;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin, MTLPixelFormat, MTLSize, MTLTexture,
    MTLTextureDescriptor,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::videotoolbox::PixelBuffer;

pub struct MetalRenderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    layer: Retained<CAMetalLayer>,
    /// Current drawable size — auto-reconfigured on every `present()` call
    /// whose framebuffer dimensions differ.
    fb_w: u32,
    fb_h: u32,
    /// Device's actual pixel dimensions, fixed at construction. Used for
    /// converting cursor→device coordinates for input injection (those want
    /// device pixels, not frame-buffer pixels).
    dev_w: u32,
    dev_h: u32,
}

impl MetalRenderer {
    pub fn new(window: &Window, dev_w: u32, dev_h: u32) -> Option<Self> {
        let fb_w = dev_w;
        let fb_h = dev_h;
        // Resolve the underlying NSView from winit (raw-window-handle 0.6).
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: winit keeps the NSView alive as long as the window.
        let ns_view: &AnyObject = unsafe { handle.ns_view.cast().as_ref() };

        let device = MTLCreateSystemDefaultDevice()?;
        let queue = device.newCommandQueue()?;
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setPresentsWithTransaction(false);
        layer.setDrawableSize(CGSize::new(fb_w as f64, fb_h as f64));

        // Attach the Metal layer to the NSView. We replace the view's layer
        // entirely; `setWantsLayer:YES` is required to opt the view into
        // layer-backed rendering before macOS will look at `layer`.
        unsafe {
            let _: () = msg_send![ns_view, setWantsLayer: true];
            let _: () = msg_send![ns_view, setLayer: &*layer];
        }

        Some(Self {
            device,
            queue,
            layer,
            fb_w,
            fb_h,
            dev_w,
            dev_h,
        })
    }

    /// Drawable size is fixed at the framebuffer (device) resolution; we don't
    /// touch it on window resize. The visible scaling happens because the
    /// layer's view bounds change with the window, and `contentsGravity` keeps
    /// the aspect ratio. This avoids reallocating the drawable per resize.
    pub fn resize_window(&self, _w: u32, _h: u32) {
        // Intentionally empty — see comment above.
    }

    fn fit_drawable(&mut self, w: u32, h: u32) {
        if w != self.fb_w || h != self.fb_h {
            self.layer.setDrawableSize(CGSize::new(w as f64, h as f64));
            self.fb_w = w;
            self.fb_h = h;
        }
    }

    /// Zero-copy present from a VideoToolbox-decoded `CVPixelBuffer`.
    ///
    /// The pixel buffer is IOSurface-backed (we set `kMetalCompatibility=true`
    /// on the decoder's destination attributes). We wrap the IOSurface as an
    /// `MTLTexture` and blit it straight into the drawable — CPU never touches
    /// the BGRA data, the decoder's media-engine writes go to the IOSurface
    /// and the GPU reads from the same IOSurface.
    pub fn present_pixel_buffer(&mut self, pb: &PixelBuffer) {
        let w = pb.width();
        let h = pb.height();
        if w == 0 || h == 0 {
            return;
        }
        let Some(src_texture) = texture_for(&self.device, pb) else {
            return;
        };
        self.fit_drawable(w, h);

        let Some(drawable) = self.layer.nextDrawable() else {
            return;
        };
        let Some(cmd_buf) = self.queue.commandBuffer() else {
            return;
        };
        let Some(blit) = cmd_buf.blitCommandEncoder() else {
            return;
        };
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &src_texture,
                0,
                0,
                origin(),
                size(w, h),
                &drawable.texture(),
                0,
                0,
                origin(),
            );
        }
        blit.endEncoding();
        cmd_buf.presentDrawable(ProtocolObject::from_ref(&*drawable));
        cmd_buf.commit();
        // `src_texture` drops here; the command buffer retains it (and
        // transitively the IOSurface) until GPU execution completes.
    }

    /// Map a cursor position (in window physical pixels) to **device** pixel
    /// coordinates, accounting for letterboxing. Returns None if outside the
    /// device-aspect rectangle within the window.
    ///
    /// Letterbox geometry uses the current `fb_w`/`fb_h` (what the layer is
    /// actually displaying), but the OUTPUT is mapped onto `dev_w`/`dev_h`
    /// because `UiAutomation.injectInputEvent` takes device pixels — not
    /// downscaled-mirror pixels.
    pub fn window_pos_to_pixel(
        &self,
        win_w: u32,
        win_h: u32,
        wx: f64,
        wy: f64,
    ) -> Option<(i32, i32)> {
        if win_w == 0 || win_h == 0 || self.fb_w == 0 || self.fb_h == 0 {
            return None;
        }
        let dw = win_w as f64;
        let dh = win_h as f64;
        let sw = self.fb_w as f64;
        let sh = self.fb_h as f64;
        let src_aspect = sw / sh;
        let dst_aspect = dw / dh;
        let (out_w, out_h) = if src_aspect > dst_aspect {
            (dw, dw / src_aspect)
        } else {
            (dh * src_aspect, dh)
        };
        let pad_x = (dw - out_w) * 0.5;
        let pad_y = (dh - out_h) * 0.5;
        let lx = wx - pad_x;
        let ly = wy - pad_y;
        if lx < 0.0 || ly < 0.0 || lx >= out_w || ly >= out_h {
            return None;
        }
        let device_x = lx * (self.dev_w as f64) / out_w;
        let device_y = ly * (self.dev_h as f64) / out_h;
        Some((device_x as i32, device_y as i32))
    }
}

/// Wraps a pixel buffer's IOSurface as an MTLTexture (no copy).
pub(super) fn texture_for(
    device: &ProtocolObject<dyn MTLDevice>, pb: &PixelBuffer,
) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
    let iosurface = pb.iosurface()?;
    let descriptor = MTLTextureDescriptor::new();
    descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    unsafe {
        descriptor.setWidth(pb.width() as usize);
        descriptor.setHeight(pb.height() as usize);
    }
    device.newTextureWithDescriptor_iosurface_plane(&descriptor, &iosurface, 0)
}

pub(super) fn origin() -> MTLOrigin {
    MTLOrigin { x: 0, y: 0, z: 0 }
}

pub(super) fn size(w: u32, h: u32) -> MTLSize {
    MTLSize { width: w as usize, height: h as usize, depth: 1 }
}
