//! Playing to an output device on Windows: a shared-mode WASAPI client in
//! the mixer's format (48 kHz stereo f32).

use windows::core::Result;
use windows::Win32::Media::Audio::{eRender, IAudioClient, IAudioRenderClient};

use crate::devices::{endpoint, endpoint_id};
use crate::loopback::standard_format;
use crate::render::open_render_client;

/// One opened output device, kept alive only while something plays on it.
pub(crate) struct DeviceOutput {
    client: IAudioClient,
    render_service: IAudioRenderClient,
    buffer_frames: u32,
    /// The device opened: for the default, whichever device that was then
    /// (the stream stays on it).
    endpoint_id: Option<String>,
}

impl DeviceOutput {
    /// Opens and starts `device_id` (`None` = the system default).
    pub(crate) fn open(device_id: Option<&str>) -> Result<Self> {
        let device = endpoint(device_id, eRender)?;
        let (client, render_service) = open_render_client(&device, &standard_format())?;
        unsafe {
            let buffer_frames = client.GetBufferSize()?;
            client.Start()?;
            Ok(Self { client, render_service, buffer_frames, endpoint_id: endpoint_id(&device).ok() })
        }
    }

    /// The device playing, as [`crate::devices::list_output_devices`] names
    /// it; `None` if Windows didn't say.
    pub(crate) fn endpoint_id(&self) -> Option<&str> {
        self.endpoint_id.as_deref()
    }

    /// The most frames the device can have queued.
    pub(crate) fn capacity_frames(&self) -> u32 {
        self.buffer_frames
    }

    /// Frames written but not played yet. An error means the device is
    /// gone (unplugged, or taken over by an exclusive-mode app).
    pub(crate) fn queued_frames(&self) -> Result<u32> {
        unsafe { self.client.GetCurrentPadding() }
    }

    /// Queues interleaved stereo `samples`; at most the room left
    /// ([`Self::capacity_frames`] minus [`Self::queued_frames`]).
    pub(crate) fn write(&mut self, samples: &[f32]) -> Result<()> {
        let frames = (samples.len() / 2) as u32;
        if frames == 0 {
            return Ok(());
        }
        unsafe {
            let buffer = self.render_service.GetBuffer(frames)? as *mut f32;
            std::ptr::copy_nonoverlapping(samples.as_ptr(), buffer, frames as usize * 2);
            self.render_service.ReleaseBuffer(frames, 0)
        }
    }
}

impl Drop for DeviceOutput {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
