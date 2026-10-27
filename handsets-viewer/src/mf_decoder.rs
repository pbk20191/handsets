// Windows H.264 decode: the Media Foundation H.264 decoder MFT bound to the
// shared D3D11 device (DXVA), so each decoded frame is an NV12 texture that
// the D3D renderer's video processor blits straight to the swap chain. Input
// is the daemon's stream_h264 framing: one Annex-B access unit (or SPS/PPS)
// per frame.
#![cfg(windows)]

use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample,
    IMFTransform, MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFStartup, MFVideoArea, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFSTARTUP_FULL, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use crate::win_gpu::Gpu;

/// A decoded frame: `slice` of the decoder's NV12 texture array, `w`×`h`
/// visible pixels. Holding the sample keeps the surface out of the
/// decoder's pool until the frame is replaced.
pub struct GpuFrame {
    pub texture: ID3D11Texture2D,
    pub slice: u32,
    pub width: u32,
    pub height: u32,
    _sample: IMFSample,
}

// The texture lives on the multithread-protected shared device; the sample
// is a free-threaded MF object.
unsafe impl Send for GpuFrame {}

pub struct MfDecoder {
    mft: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    size: (u32, u32),
    pts: i64,
}

impl MfDecoder {
    pub fn new(gpu: &Gpu) -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;

            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            manager.ResetDevice(&gpu.device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            if let Ok(attrs) = mft.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            mft.SetInputType(0, &input, 0)?;

            let mut dec = Self { mft, _manager: manager, size: (0, 0), pts: 0 };
            dec.select_nv12_output()?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let info = dec.mft.GetOutputStreamInfo(0)?;
            if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                // Without a D3D-backed output the decoder wants caller samples
                // in system memory; this path only handles GPU surfaces.
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_NOTIMPL,
                    "H.264 decoder is not hardware-backed on this machine",
                ));
            }
            Ok(dec)
        }
    }

    /// Feeds one access unit; returns the newest frame the decoder produced, if any.
    pub fn decode(&mut self, au: &[u8]) -> windows::core::Result<Option<GpuFrame>> {
        unsafe {
            let buf = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), ptr, au.len());
            buf.Unlock()?;
            buf.SetCurrentLength(au.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            // 100 ns units; the rate only orders frames for the decoder.
            sample.SetSampleTime(self.pts * 166_667)?;
            sample.SetSampleDuration(166_667)?;
            self.pts += 1;

            let mut newest = self.drain()?;
            self.mft.ProcessInput(0, &sample, 0)?;
            if let Some(f) = self.drain()? {
                newest = Some(f);
            }
            Ok(newest)
        }
    }

    /// Pulls every ready output frame, keeping the last.
    unsafe fn drain(&mut self) -> windows::core::Result<Option<GpuFrame>> {
        let mut newest = None;
        loop {
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            let _events = ManuallyDrop::take(&mut out[0].pEvents);
            match r {
                Ok(()) => {
                    if let Some(s) = sample {
                        newest = Some(self.frame_of(s)?);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(newest),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.select_nv12_output()?,
                Err(e) => return Err(e),
            }
        }
    }

    unsafe fn frame_of(&self, sample: IMFSample) -> windows::core::Result<GpuFrame> {
        let dxgi: IMFDXGIBuffer = sample.GetBufferByIndex(0)?.cast()?;
        let mut raw = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
        let texture = ID3D11Texture2D::from_raw(raw);
        let slice = dxgi.GetSubresourceIndex()?;
        Ok(GpuFrame { texture, slice, width: self.size.0, height: self.size.1, _sample: sample })
    }

    /// Picks NV12 output (needed again after every stream change) and
    /// records the visible size.
    unsafe fn select_nv12_output(&mut self) -> windows::core::Result<()> {
        let mut i = 0;
        loop {
            let t: IMFMediaType = self.mft.GetOutputAvailableType(0, i)?;
            if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                self.mft.SetOutputType(0, &t, 0)?;
                self.size = visible_size(&t);
                return Ok(());
            }
            i += 1;
        }
    }
}

/// Minimum display aperture if present (the coded size is padded to 16),
/// otherwise the frame size.
unsafe fn visible_size(t: &IMFMediaType) -> (u32, u32) {
    let mut area = MFVideoArea::default();
    let bytes = std::slice::from_raw_parts_mut(
        (&mut area as *mut MFVideoArea).cast::<u8>(),
        std::mem::size_of::<MFVideoArea>(),
    );
    if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok() && area.Area.cx > 0 {
        return (area.Area.cx as u32, area.Area.cy as u32);
    }
    let packed = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
    ((packed >> 32) as u32, packed as u32)
}
