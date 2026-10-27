// macOS hardware H.264 and JPEG decoders using VideoToolbox.
//
// The H.264 decoder consumes the same Annex-B packets the Android
// H264Streamer emits (length-prefixed NAL access units); the JPEG decoder
// takes the `stream` frames whole. Both emit IOSurface-backed CVPixelBuffers
// that the Metal renderer wraps as MTLTextures — CPU never touches BGRA pixel
// data once it leaves the network buffer.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{
    kCFAllocatorNull, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMVideoCodecType_JPEG, CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreate, CMVideoFormatDescriptionCreateFromH264ParameterSets,
};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA, CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight,
    CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
};
use objc2_io_surface::IOSurfaceRef;
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

type OSStatus = i32;

// ---------- decoded frame ----------

/// A decoded frame: a retained, IOSurface-backed BGRA `CVPixelBuffer`.
#[derive(Clone)]
pub struct PixelBuffer(pub(super) CFRetained<CVPixelBuffer>);

impl PixelBuffer {
    /// IOSurface backing the pixel buffer. None only if the buffer is not
    /// IOSurface-backed (shouldn't happen — we set kMetalCompatibility=true).
    pub fn iosurface(&self) -> Option<CFRetained<IOSurfaceRef>> {
        CVPixelBufferGetIOSurface(Some(&self.0))
    }

    pub fn width(&self) -> u32 {
        CVPixelBufferGetWidth(&self.0) as u32
    }

    pub fn height(&self) -> u32 {
        CVPixelBufferGetHeight(&self.0) as u32
    }
}

// CVPixelBuffer ref-counts are atomic and the decoder is done writing by the
// time we hand one out, so moving it to the render thread is fine.
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

// ---------- shared session plumbing ----------

/// A decompression session plus the format it was built for. Dropping it
/// invalidates the session.
struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

// The session is only used from the thread that owns the decoder.
unsafe impl Send for Session {}

struct DecodedSlot {
    pb: Option<PixelBuffer>,
    status: OSStatus,
}

unsafe extern "C-unwind" fn vt_output_callback(
    _decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    if source_frame_ref_con.is_null() {
        return;
    }
    // SAFETY: `Session::decode` passes a stack pointer to a DecodedSlot that
    // lives across DecodeFrame + WaitForAsynchronousFrames.
    let slot = unsafe { &mut *(source_frame_ref_con as *mut DecodedSlot) };
    slot.status = status;
    if status == 0 {
        // VT hands the buffer over unretained; keep our own reference.
        slot.pb = NonNull::new(image_buffer).map(|p| PixelBuffer(unsafe { CFRetained::retain(p) }));
    }
}

/// Pixel-buffer attributes for everything we hand to Metal:
///   kCVPixelBufferPixelFormatTypeKey      = 32BGRA
///   kCVPixelBufferMetalCompatibilityKey  = true  (forces IOSurface backing
///                                                 usable as MTLTexture)
/// plus a fixed size when `size` is given (for buffer pools).
pub(super) fn bgra_attributes(size: Option<(u32, u32)>) -> CFRetained<CFDictionary<CFString, CFType>> {
    let pixel_format = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
    let mut keys: Vec<&CFString> =
        unsafe { vec![kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferMetalCompatibilityKey] };
    let mut values: Vec<&CFType> = vec![&pixel_format, CFBoolean::new(true)];
    let dims = size.map(|(w, h)| (CFNumber::new_i32(w as i32), CFNumber::new_i32(h as i32)));
    if let Some((w, h)) = &dims {
        unsafe { keys.extend([kCVPixelBufferWidthKey, kCVPixelBufferHeightKey]) };
        values.extend([&**w as &CFType, &**h]);
    }
    // Pool buffers need this to be IOSurface-backed; VT implies it already.
    let surface_props = CFDictionary::<CFString, CFType>::empty();
    unsafe { keys.push(kCVPixelBufferIOSurfacePropertiesKey) };
    values.push(&**surface_props.as_opaque());
    CFDictionary::from_slices(&keys, &values)
}

impl Session {
    /// A session for `format` that writes Metal-compatible BGRA pixel buffers.
    fn new(format: CFRetained<CMFormatDescription>) -> Result<Self, String> {
        let dest = bgra_attributes(None);

        let cb = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(vt_output_callback),
            decompressionOutputRefCon: ptr::null_mut(),
        };
        let mut session: *mut VTDecompressionSession = ptr::null_mut();
        let s = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(dest.as_opaque()),
                &cb,
                NonNull::from(&mut session),
            )
        };
        let session = NonNull::new(session)
            .filter(|_| s == 0)
            .ok_or_else(|| format!("VTDecompressionSessionCreate: {s}"))?;
        // SAFETY: Create returns +1.
        Ok(Self { session: unsafe { CFRetained::from_raw(session) }, format })
    }

    /// Decodes one sample synchronously; `data` only has to live for the call.
    fn decode(&self, data: &[u8]) -> Result<Option<PixelBuffer>, String> {
        unsafe {
            // Wrap the bytes in a CMBlockBuffer with kCFAllocatorNull as the
            // block allocator — VT must NOT try to free Rust-owned memory,
            // and only reads it.
            let mut block: *mut CMBlockBuffer = ptr::null_mut();
            let s = CMBlockBuffer::create_with_memory_block(
                None,
                data.as_ptr() as *mut c_void,
                data.len(),
                kCFAllocatorNull,
                ptr::null(),
                0,
                data.len(),
                0,
                NonNull::from(&mut block),
            );
            let block = NonNull::new(block)
                .filter(|_| s == 0)
                .ok_or_else(|| format!("CMBlockBufferCreateWithMemoryBlock: {s}"))?;
            let block = CFRetained::from_raw(block);

            let sizes: [usize; 1] = [data.len()];
            let mut sample: *mut CMSampleBuffer = ptr::null_mut();
            let s = CMSampleBuffer::create(
                None,
                Some(&block),
                true,
                None,
                ptr::null_mut(),
                Some(&self.format),
                1, // numSamples
                0, // no timing info — we present immediately
                ptr::null(),
                1,
                sizes.as_ptr(),
                NonNull::from(&mut sample),
            );
            let sample = NonNull::new(sample)
                .filter(|_| s == 0)
                .ok_or_else(|| format!("CMSampleBufferCreate: {s}"))?;
            let sample = CFRetained::from_raw(sample);

            let mut slot = DecodedSlot { pb: None, status: 0 };
            let mut info_flags = VTDecodeInfoFlags(0);
            let s = self.session.decode_frame(
                &sample,
                VTDecodeFrameFlags(0), // no special flags — default sync-ish behaviour
                &mut slot as *mut _ as *mut c_void,
                &mut info_flags,
            );
            if s != 0 {
                return Err(format!("VTDecompressionSessionDecodeFrame: {s}"));
            }
            // Flush any deferred decodes so the callback has run before we
            // return — the caller may reuse `data` while VT would still
            // reference it.
            self.session.wait_for_asynchronous_frames();

            if slot.status != 0 {
                return Err(format!("VT decode callback status: {}", slot.status));
            }
            Ok(slot.pb)
        }
    }
}

// ---------- H.264 ----------

pub struct VtDecoder {
    session: Option<Session>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    /// Re-used scratch buffer for converting Annex-B start codes to AVCC
    /// 4-byte length prefixes per access unit.
    avcc_buf: Vec<u8>,
}

impl VtDecoder {
    pub fn new() -> Self {
        Self {
            session: None,
            sps: None,
            pps: None,
            avcc_buf: Vec::with_capacity(256 * 1024),
        }
    }

    /// Decode a length-prefixed Annex-B packet from the wire. Returns Some(pb)
    /// when a frame is produced, None for codec-config-only packets (SPS/PPS
    /// before the first IDR).
    pub fn decode(&mut self, packet: &[u8]) -> Result<Option<PixelBuffer>, String> {
        self.avcc_buf.clear();
        let mut new_params = false;
        for nal in AnnexBIter::new(packet) {
            if nal.is_empty() {
                continue;
            }
            let nal_type = nal[0] & 0x1f;
            match nal_type {
                7 => {
                    // SPS — store; only flag a rebuild if it changed.
                    if self.sps.as_deref() != Some(nal) {
                        self.sps = Some(nal.to_vec());
                        new_params = true;
                    }
                }
                8 => {
                    if self.pps.as_deref() != Some(nal) {
                        self.pps = Some(nal.to_vec());
                        new_params = true;
                    }
                }
                9 => {
                    // Access unit delimiter — drop, not part of the AVCC frame
                    // data the decoder wants.
                }
                _ => {
                    // Slice / SEI / others → AVCC with 4-byte big-endian length.
                    let len = nal.len() as u32;
                    self.avcc_buf.extend_from_slice(&len.to_be_bytes());
                    self.avcc_buf.extend_from_slice(nal);
                }
            }
        }

        if new_params {
            if let (Some(sps), Some(pps)) = (&self.sps, &self.pps) {
                self.session = None;
                self.session = Some(Session::new(h264_format(sps, pps)?)?);
            }
        }
        match &self.session {
            Some(session) if !self.avcc_buf.is_empty() => session.decode(&self.avcc_buf),
            _ => Ok(None),
        }
    }
}

fn h264_format(sps: &[u8], pps: &[u8]) -> Result<CFRetained<CMFormatDescription>, String> {
    let pointers = [NonNull::from(&sps[0]), NonNull::from(&pps[0])];
    let sizes = [sps.len(), pps.len()];
    let mut fmt: *const CMFormatDescription = ptr::null();
    let s = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            2,
            NonNull::from(&pointers[0]),
            NonNull::from(&sizes[0]),
            4,
            NonNull::from(&mut fmt),
        )
    };
    let fmt = NonNull::new(fmt as *mut CMFormatDescription)
        .filter(|_| s == 0)
        .ok_or_else(|| format!("CMVideoFormatDescriptionCreateFromH264ParameterSets: {s}"))?;
    // SAFETY: Create returns +1.
    Ok(unsafe { CFRetained::from_raw(fmt) })
}

// ---------- JPEG ----------

/// Hardware JPEG decode: VideoToolbox's Motion-JPEG decoder. A session is
/// tied to one frame size, so a few are kept: the full frame (both
/// orientations) and, for tilejpeg, the inner and edge tile sizes.
pub struct VtJpegDecoder {
    /// Most recently used last.
    sessions: Vec<((i32, i32), Session)>,
}

impl VtJpegDecoder {
    const MAX_SESSIONS: usize = 8;

    pub fn new() -> Self {
        Self { sessions: Vec::new() }
    }

    pub fn decode(&mut self, jpeg: &[u8]) -> Result<PixelBuffer, String> {
        let size = jpeg_size(jpeg).ok_or("no JPEG frame header")?;
        match self.sessions.iter().position(|(s, _)| *s == size) {
            Some(i) => {
                let entry = self.sessions.remove(i);
                self.sessions.push(entry);
            }
            None => {
                if self.sessions.len() == Self::MAX_SESSIONS {
                    self.sessions.remove(0);
                }
                self.sessions.push((size, Session::new(jpeg_format(size)?)?));
            }
        }
        let (_, session) = self.sessions.last().unwrap();
        session.decode(jpeg)?.ok_or_else(|| "VT produced no image".into())
    }
}

fn jpeg_format((w, h): (i32, i32)) -> Result<CFRetained<CMFormatDescription>, String> {
    let mut fmt: *const CMFormatDescription = ptr::null();
    let s = unsafe {
        CMVideoFormatDescriptionCreate(None, kCMVideoCodecType_JPEG, w, h, None, NonNull::from(&mut fmt))
    };
    let fmt = NonNull::new(fmt as *mut CMFormatDescription)
        .filter(|_| s == 0)
        .ok_or_else(|| format!("CMVideoFormatDescriptionCreate: {s}"))?;
    // SAFETY: Create returns +1.
    Ok(unsafe { CFRetained::from_raw(fmt) })
}

/// Width and height from the first SOF marker.
fn jpeg_size(jpeg: &[u8]) -> Option<(i32, i32)> {
    let mut i = 2;
    while i + 9 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let marker = jpeg[i + 1];
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        // SOF0..SOF15, minus DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]) as i32;
            let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]) as i32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

// ---------- Annex-B NAL splitter ----------

struct AnnexBIter<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> AnnexBIter<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl<'a> Iterator for AnnexBIter<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        let (_, nal_begin) = find_start_code(self.buf, self.pos)?;
        let nal_end = match find_start_code(self.buf, nal_begin) {
            Some((next_start, _)) => next_start,
            None => self.buf.len(),
        };
        self.pos = nal_end;
        if nal_begin >= nal_end {
            return None;
        }
        Some(&self.buf[nal_begin..nal_end])
    }
}

/// Returns (start_index, byte_after_start_code) for the next 3- or 4-byte
/// Annex-B start code at or after `from`.
fn find_start_code(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if i + 4 <= buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((i, i + 4));
            }
            if buf[i + 2] == 1 {
                return Some((i, i + 3));
            }
        }
        i += 1;
    }
    None
}
