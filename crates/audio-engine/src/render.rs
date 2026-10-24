use windows::core::Result;
use windows::Win32::Media::Audio::{
    eRender, IAudioClient, IAudioRenderClient, IMMDevice, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
};
use windows::Win32::System::Com::CLSCTX_ALL;

use crate::devices::endpoint;

/// 200ms shared-mode buffer: generous enough to avoid glitches on a
/// background thread without adding noticeable latency for this prototype.
const BUFFER_DURATION_100NS: i64 = 2_000_000;

/// Opens the system default playback device in shared mode with
/// [`crate::loopback::standard_format`], ready to `Start()` and render into.
pub fn open_default_render_client(format: &WAVEFORMATEX) -> Result<(IAudioClient, IAudioRenderClient)> {
    open_render_client_for_device(None, format)
}

/// Opens `device_id` (as returned by [`crate::devices::list_output_devices`])
/// in shared mode, or the system default device when `device_id` is `None`.
pub fn open_render_client_for_device(
    device_id: Option<&str>,
    format: &WAVEFORMATEX,
) -> Result<(IAudioClient, IAudioRenderClient)> {
    open_render_client(&endpoint(device_id, eRender)?, format)
}

/// Opens `device` in shared mode with `format`, ready to `Start()`.
pub(crate) fn open_render_client(device: &IMMDevice, format: &WAVEFORMATEX) -> Result<(IAudioClient, IAudioRenderClient)> {
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        // A device not at 48 kHz (a virtual cable left at 44.1 kHz, say) is
        // converted by Windows; its better converter, for listening.
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_DURATION_100NS,
            0,
            format,
            None,
        )?;
        let render_client: IAudioRenderClient = client.GetService()?;
        Ok((client, render_client))
    }
}
