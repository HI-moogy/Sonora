//! Capturing one window (a game) instead of the whole monitor, with
//! Windows.Graphics.Capture: only that window's own content, even while
//! other windows cover it, and nothing else on screen.

use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::duplication::{create_image, Poll};

/// Frames the pool holds; the recorder only ever wants the latest.
const POOL_FRAMES: i32 = 2;

/// Keeps the latest image of one window in `latest`, like
/// [`crate::duplication::Desktop`] does for the monitor. The image is the
/// window's size and follows it as it's resized.
pub(crate) struct WindowCapture {
    pub window: isize,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pool_size: SizeInt32,
    pub latest: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
}

impl WindowCapture {
    pub(crate) fn new(device: ID3D11Device, context: ID3D11DeviceContext, window: isize) -> windows::core::Result<Self> {
        let item: GraphicsCaptureItem = unsafe {
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?.CreateForWindow(HWND(window as _))?
        };
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)?.cast()? };
        let pool_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            POOL_FRAMES,
            pool_size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Not available on every Windows version; the defaults are fine.
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;
        let (width, height) = (pool_size.Width.max(1) as u32, pool_size.Height.max(1) as u32);
        let latest = create_image(&device, width, height)?;
        Ok(Self {
            window,
            device,
            context,
            winrt_device,
            pool,
            session,
            pool_size,
            latest,
            width,
            height,
        })
    }

    /// Whether the window still exists.
    pub(crate) fn is_open(&self) -> bool {
        unsafe { IsWindow(Some(HWND(self.window as _))) }.as_bool()
    }

    /// Takes the newest frame, if any arrived, into `latest`. Windows sends
    /// frames only when the window changes, so `latest` is re-encoded at
    /// the recorder's own rate either way.
    pub(crate) fn poll(&mut self) -> windows::core::Result<Poll> {
        let mut newest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            newest = Some(frame);
        }
        let Some(frame) = newest else {
            return Ok(Poll::Unchanged);
        };
        let content = frame.ContentSize()?;
        let (width, height) = (content.Width.max(1) as u32, content.Height.max(1) as u32);

        let mut result = Poll::Updated;
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            // The window was resized: frames from now on are the new size.
            self.pool.Recreate(&self.winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, POOL_FRAMES, content)?;
            self.pool_size = content;
        }
        if (width, height) != (self.width, self.height) {
            self.latest = create_image(&self.device, width, height)?;
            (self.width, self.height) = (width, height);
            result = Poll::Resized;
        }

        let surface: ID3D11Texture2D =
            unsafe { frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()? };
        // The surface can be larger than the content while a resize settles.
        let region = D3D11_BOX { left: 0, top: 0, front: 0, right: width, bottom: height, back: 1 };
        unsafe {
            self.context.CopySubresourceRegion(&self.latest, 0, 0, 0, 0, &surface, 0, Some(&region));
        }
        Ok(result)
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
