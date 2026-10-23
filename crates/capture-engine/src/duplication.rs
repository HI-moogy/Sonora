use std::time::{Duration, Instant};

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

/// How often to retry duplication while it's unavailable (secure desktop,
/// lock screen, mid mode-switch).
const RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Keeps the latest image of the primary monitor in `latest`, a texture this
/// struct owns. Desktop duplication only hands out a frame when the screen
/// changes, so callers re-encode `latest` at their own fixed rate.
pub(crate) struct Desktop {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    output: IDXGIOutput5,
    duplication: Option<IDXGIOutputDuplication>,
    last_attempt: Instant,
    pub latest: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
}

/// What a `poll` call changed.
#[derive(PartialEq, Eq)]
pub(crate) enum Poll {
    Unchanged,
    /// `latest` holds a newer desktop image.
    Updated,
    /// The desktop resolution changed and `latest` was recreated at the new
    /// size; anything holding views of the old texture must rebuild them.
    Resized,
}

impl Desktop {
    pub(crate) fn new(device: ID3D11Device, context: ID3D11DeviceContext, output: IDXGIOutput5) -> Result<Self> {
        let duplication = duplicate(&device, &output)?;
        let (width, height) = mode_size(&duplication);
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
        let updated = info.LastPresentTime != 0;
        if updated {
            if let Some(resource) = &resource {
                let frame: ID3D11Texture2D = resource.cast()?;
                unsafe { self.context.CopyResource(&self.latest, &frame) };
            }
        }
        drop(resource);
        unsafe { duplication.ReleaseFrame()? };
        Ok(if updated { Poll::Updated } else { Poll::Unchanged })
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
        let (width, height) = mode_size(&duplication);
        self.duplication = Some(duplication);
        if (width, height) == (self.width, self.height) {
            return Ok(Poll::Unchanged);
        }
        self.latest = create_image(&self.device, width, height)?;
        self.width = width;
        self.height = height;
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

fn mode_size(duplication: &IDXGIOutputDuplication) -> (u32, u32) {
    let desc = unsafe { duplication.GetDesc() };
    (desc.ModeDesc.Width, desc.ModeDesc.Height)
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
