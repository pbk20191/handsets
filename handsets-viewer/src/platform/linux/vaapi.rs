// Linux H.264: libavcodec with a VA-API hardware device. Each decoded VA
// surface is mapped to DRM PRIME (dma-buf fds + plane layout) with
// av_hwframe_map, which the GL renderer imports through
// EGL_EXT_image_dma_buf_import — no copy through system memory.

use std::ptr;

use ffmpeg_sys_next as ff;

/// AVERROR(EAGAIN) on Linux.
const AVERROR_EAGAIN: i32 = -11;

/// A decoded frame as DRM PRIME; dropping it releases the mapping and the
/// underlying VA surface.
pub struct GpuFrame {
    frame: *mut ff::AVFrame,
    pub width: u32,
    pub height: u32,
}

// The frame only carries dma-buf fds and refcounted buffers; it is handed
// from the decoder thread to the UI thread once and never shared.
unsafe impl Send for GpuFrame {}

impl GpuFrame {
    pub fn descriptor(&self) -> &ff::AVDRMFrameDescriptor {
        unsafe { &*((*self.frame).data[0] as *const ff::AVDRMFrameDescriptor) }
    }
}

impl Drop for GpuFrame {
    fn drop(&mut self) {
        unsafe { ff::av_frame_free(&mut self.frame) };
    }
}

pub struct VaapiDecoder {
    ctx: *mut ff::AVCodecContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
}

unsafe impl Send for VaapiDecoder {}

unsafe extern "C" fn pick_vaapi(
    _ctx: *mut ff::AVCodecContext, mut fmts: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    while *fmts != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *fmts == ff::AVPixelFormat::AV_PIX_FMT_VAAPI {
            return *fmts;
        }
        fmts = fmts.add(1);
    }
    // No VA-API surface format offered: fail rather than decode on the CPU.
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

impl VaapiDecoder {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let codec = ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_H264);
            if codec.is_null() {
                return Err("libavcodec has no H.264 decoder".into());
            }
            let mut device: *mut ff::AVBufferRef = ptr::null_mut();
            let r = ff::av_hwdevice_ctx_create(
                &mut device,
                ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                ptr::null(),
                ptr::null_mut(),
                0,
            );
            if r < 0 {
                return Err(format!("no VA-API device (error {r})"));
            }
            let ctx = ff::avcodec_alloc_context3(codec);
            (*ctx).hw_device_ctx = ff::av_buffer_ref(device);
            ff::av_buffer_unref(&mut device);
            (*ctx).get_format = Some(pick_vaapi);
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as i32;
            let r = ff::avcodec_open2(ctx, codec, ptr::null_mut());
            if r < 0 {
                let mut ctx = ctx;
                ff::avcodec_free_context(&mut ctx);
                return Err(format!("avcodec_open2 failed (error {r})"));
            }
            Ok(Self { ctx, packet: ff::av_packet_alloc(), frame: ff::av_frame_alloc() })
        }
    }

    /// Feeds one access unit; returns the newest decoded frame, if any.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<GpuFrame>, String> {
        unsafe {
            (*self.packet).data = au.as_ptr() as *mut u8;
            (*self.packet).size = au.len() as i32;
            let r = ff::avcodec_send_packet(self.ctx, self.packet);
            (*self.packet).data = ptr::null_mut();
            (*self.packet).size = 0;
            if r < 0 && r != AVERROR_EAGAIN {
                return Err(format!("avcodec_send_packet failed (error {r})"));
            }
            let mut newest = None;
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame);
                if r == AVERROR_EAGAIN || r == ff::AVERROR_EOF {
                    return Ok(newest);
                }
                if r < 0 {
                    return Err(format!("avcodec_receive_frame failed (error {r})"));
                }
                if (*self.frame).format != ff::AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
                    ff::av_frame_unref(self.frame);
                    return Err("decoder produced a non-VA-API frame".into());
                }
                let mut drm = ff::av_frame_alloc();
                (*drm).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
                let r = ff::av_hwframe_map(drm, self.frame, ff::AV_HWFRAME_MAP_READ as i32);
                let (width, height) = ((*self.frame).width as u32, (*self.frame).height as u32);
                ff::av_frame_unref(self.frame);
                if r < 0 {
                    ff::av_frame_free(&mut drm);
                    return Err(format!("VA-API → DRM PRIME mapping failed (error {r})"));
                }
                newest = Some(GpuFrame { frame: drm, width, height });
            }
        }
    }
}

impl Drop for VaapiDecoder {
    fn drop(&mut self) {
        unsafe {
            ff::av_frame_free(&mut self.frame);
            ff::av_packet_free(&mut self.packet);
            ff::avcodec_free_context(&mut self.ctx);
        }
    }
}
