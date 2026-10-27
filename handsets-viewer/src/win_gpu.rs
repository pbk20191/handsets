// One D3D11 device for the whole viewer on Windows: the swap-chain renderer
// (UI thread) and the Media Foundation H.264 decoder (network thread) share
// it, so decoded NV12 textures reach the screen without leaving the GPU.
// Multithread protection lets both threads use the immediate context.
#![cfg(windows)]

use std::sync::OnceLock;

use windows::core::Interface;
use windows::Win32::Foundation::{E_FAIL, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11VideoContext,
    ID3D11VideoDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION,
};

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub video_device: ID3D11VideoDevice,
    pub video_context: ID3D11VideoContext,
}

// The immediate context is multithread-protected (see `create`), and the
// device and video interfaces are free-threaded.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

/// The shared device, created on first use; None if the machine has no
/// usable hardware D3D11 device with video support.
pub fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match create() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("viewer: D3D11 device unavailable: {e}");
            None
        }
    })
    .as_ref()
}

fn create() -> windows::core::Result<Gpu> {
    let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
    let context = context.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
    unsafe {
        let _ = context.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
    }
    let video_device: ID3D11VideoDevice = device.cast()?;
    let video_context: ID3D11VideoContext = context.cast()?;
    Ok(Gpu { device, context, video_device, video_context })
}
