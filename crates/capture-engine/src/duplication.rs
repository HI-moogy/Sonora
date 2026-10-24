use std::time::{Duration, Instant};

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_ROTATE90,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

/// How often to retry duplication while it's unavailable (secure desktop,
/// lock screen, mid mode-switch).
const RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Keeps the latest image of a monitor in `latest`, a texture this struct
/// owns. Desktop duplication only hands out a frame when the screen
/// changes, so callers re-encode `latest` at their own fixed rate.
///
/// Frames come as the monitor scans them out: a monitor turned on its side
/// gives sideways frames (1920x1080 for a 1080x1920 desktop), so `latest`
/// is that size and `rotation` says how the desktop was turned.
pub(crate) struct Desktop {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output: IDXGIOutput5,
    duplication: Option<IDXGIOutputDuplication>,
    last_attempt: Instant,
    pub latest: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub rotation: DXGI_MODE_ROTATION,
}

/// What a `poll` call changed.
#[derive(PartialEq, Eq)]
pub(crate) enum Poll {
    Unchanged,
    /// `latest` holds a newer desktop image.
    Updated,
    /// The desktop resolution (or rotation) changed and `latest` was
    /// recreated; anything holding views of the old texture, or sized from
    /// it, must rebuild.
    Resized,
}

impl Desktop {
    pub(crate) fn new(device: ID3D11Device, context: ID3D11DeviceContext, output: IDXGIOutput5) -> Result<Self> {
        let duplication = duplicate(&device, &output)?;
        let ((width, height), rotation) = frame_size(&duplication);
        let latest = create_image(&device, width, height)?;
        Ok(Self {
            device,
            context,
            output,
            duplication: Some(duplication),
            last_attempt: Instant::now(),
            latest,
            width,
            height,
            rotation,
        })
    }

    /// Waits up to `timeout` for the desktop to change and copies any new
    /// image into `latest`.
    pub(crate) fn poll(&mut self, timeout: Duration) -> Result<Poll> {
        let Some(duplication) = self.duplication.clone() else {
            return self.reconnect(timeout);
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Poll::Unchanged),
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                self.duplication = None;
                return Ok(Poll::Unchanged);
            }
            Err(error) => return Err(error),
        }

        // LastPresentTime is zero when only the mouse pointer moved.
        let mut polled = if info.LastPresentTime != 0 { Poll::Updated } else { Poll::Unchanged };
        if let (Poll::Updated, Some(resource)) = (&polled, &resource) {
            let frame: ID3D11Texture2D = resource.cast()?;
            // CopyResource does nothing if the sizes differ, which would
            // leave the image black: follow the frames' own size.
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { frame.GetDesc(&mut desc) };
            if (desc.Width, desc.Height) != (self.width, self.height) {
                self.latest = create_image(&self.device, desc.Width, desc.Height)?;
                (self.width, self.height) = (desc.Width, desc.Height);
                polled = Poll::Resized;
            }
            unsafe { self.context.CopyResource(&self.latest, &frame) };
        }
        drop(resource);
        unsafe { duplication.ReleaseFrame()? };
        Ok(polled)
    }

    fn reconnect(&mut self, timeout: Duration) -> Result<Poll> {
        if self.last_attempt.elapsed() < RETRY_INTERVAL {
            std::thread::sleep(timeout);
            return Ok(Poll::Unchanged);
        }
        self.last_attempt = Instant::now();
        let Ok(duplication) = duplicate(&self.device, &self.output) else {
            std::thread::sleep(timeout);
            return Ok(Poll::Unchanged);
        };
        let ((width, height), rotation) = frame_size(&duplication);
        self.duplication = Some(duplication);
        if (width, height, rotation) == (self.width, self.height, self.rotation) {
            return Ok(Poll::Unchanged);
        }
        self.latest = create_image(&self.device, width, height)?;
        self.width = width;
        self.height = height;
        self.rotation = rotation;
        Ok(Poll::Resized)
    }
}

fn duplicate(device: &ID3D11Device, output: &IDXGIOutput5) -> Result<IDXGIOutputDuplication> {
    // DuplicateOutput1 converts HDR desktops to the requested 8-bit format,
    // but only works in per-monitor DPI aware processes. Plain DuplicateOutput
    // works anywhere except on HDR desktops.
    match unsafe { output.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) } {
        Err(error) if error.code() == DXGI_ERROR_UNSUPPORTED => unsafe { output.DuplicateOutput(device) },
        result => result,
    }
}

/// The size frames come in, and how the desktop is turned. The mode is
/// the desktop's size, so for a monitor on its side the frames are the
/// mode turned back.
fn frame_size(duplication: &IDXGIOutputDuplication) -> ((u32, u32), DXGI_MODE_ROTATION) {
    let desc = unsafe { duplication.GetDesc() };
    let (width, height) = (desc.ModeDesc.Width, desc.ModeDesc.Height);
    let size = match desc.Rotation {
        DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270 => (height, width),
        _ => (width, height),
    };
    (size, desc.Rotation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::d3d::open_primary_output;
    use windows::Win32::Graphics::Direct3D11::{D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_USAGE_STAGING};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;

    /// Duplicates each monitor on the recording graphics card and says what
    /// Windows hands back: `cargo test -p capture-engine --lib
    /// duplicates_each_monitor -- --ignored --nocapture`.
    #[test]
    #[ignore = "captures this PC's monitors"]
    fn duplicates_each_monitor() {
        let primary = open_primary_output().expect("primary output");
        let adapter = unsafe { primary.device.cast::<IDXGIDevice>().unwrap().GetAdapter().unwrap() };
        let mut index = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(index) } {
            index += 1;
            let desc = unsafe { output.GetDesc().unwrap() };
            let name = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').to_string();
            let output: IDXGIOutput5 = output.cast().unwrap();
            let duplication = match duplicate(&primary.device, &output) {
                Ok(duplication) => duplication,
                Err(error) => {
                    println!("{name}: can't duplicate: {error}");
                    continue;
                }
            };
            let dupl = unsafe { duplication.GetDesc() };
            println!(
                "{name}: desktop {:?}, rotation {:?}, mode {}x{}",
                desc.DesktopCoordinates, dupl.Rotation, dupl.ModeDesc.Width, dupl.ModeDesc.Height
            );
            for _ in 0..20 {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource = None;
                if unsafe { duplication.AcquireNextFrame(100, &mut info, &mut resource) }.is_err() {
                    continue;
                }
                let frame: ID3D11Texture2D = resource.unwrap().cast().unwrap();
                let mut frame_desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { frame.GetDesc(&mut frame_desc) };
                if info.LastPresentTime != 0 {
                    let staging_desc = D3D11_TEXTURE2D_DESC {
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        MiscFlags: 0,
                        ..frame_desc
                    };
                    let mut staging = None;
                    unsafe { primary.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).unwrap() };
                    let staging = staging.unwrap();
                    unsafe { primary.context.CopyResource(&staging, &frame) };
                    let mut mapped = Default::default();
                    unsafe { primary.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap() };
                    let bytes = unsafe {
                        std::slice::from_raw_parts(mapped.pData as *const u8, (mapped.RowPitch * frame_desc.Height) as usize)
                    };
                    let lit = bytes.chunks(4).filter(|pixel| pixel[..3].iter().any(|&c| c > 16)).count();
                    unsafe { primary.context.Unmap(&staging, 0) };
                    println!(
                        "  frame {}x{} format {:?}: {:.0}% of pixels lit",
                        frame_desc.Width,
                        frame_desc.Height,
                        frame_desc.Format,
                        lit as f64 * 100.0 / (frame_desc.Width * frame_desc.Height) as f64
                    );
                    unsafe { duplication.ReleaseFrame().unwrap() };
                    break;
                }
                unsafe { duplication.ReleaseFrame().unwrap() };
            }
        }
    }
}

pub(crate) fn create_image(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    Ok(texture.expect("CreateTexture2D succeeded without a texture"))
}
