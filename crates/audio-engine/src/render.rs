use windows::core::{Result, PCWSTR};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

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
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device: IMMDevice = match device_id {
            Some(id) => {
                let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))?
            }
            None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?,
        };
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            BUFFER_DURATION_100NS,
            0,
            format,
            None,
        )?;
        let render_client: IAudioRenderClient = client.GetService()?;
        Ok((client, render_client))
    }
}
