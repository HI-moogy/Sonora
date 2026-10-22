use windows::core::{Interface, Result};
use windows::Win32::Foundation::{HMODULE, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput5};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};

use crate::StartError;

/// A D3D11 device on the GPU that drives the primary monitor, plus that
/// monitor's DXGI output. Desktop duplication requires the device and the
/// output to share an adapter.
pub(crate) struct PrimaryOutput {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub output: IDXGIOutput5,
}

pub(crate) fn open_primary_output() -> std::result::Result<PrimaryOutput, StartError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };

    let mut adapter_index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let mut output_index = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            let desc = unsafe { output.GetDesc()? };
            if desc.Monitor == primary {
                let (device, context) = create_device(Some(&adapter.cast()?))?;
                return Ok(PrimaryOutput { device, context, output: output.cast()? });
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    Err(StartError::NoPrimaryOutput)
}

/// A video-capable device on the default GPU.
pub(crate) fn create_default_device() -> Result<ID3D11Device> {
    create_device(None).map(|(device, _)| device)
}

fn create_device(
    adapter: Option<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            // A given adapter needs UNKNOWN; the default one needs HARDWARE.
            if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device: ID3D11Device = device.expect("D3D11CreateDevice succeeded without a device");
    let context = context.expect("D3D11CreateDevice succeeded without a context");
    // The hardware encoder and decoder use the device from their own threads.
    unsafe {
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
    }
    Ok((device, context))
}
