//! Desktop audio: everything playing on the default output device, captured
//! with WASAPI loopback into the recorder's [`DESKTOP_TRACK`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

use crate::audio::{AudioInput, AudioShared, CHANNELS, DESKTOP_TRACK, SAMPLE_RATE};
use crate::com::MfThread;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How often to check whether the default output device changed.
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// WAVE_FORMAT_IEEE_FLOAT (mmreg.h).
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// Runs on the "moments-desktop" thread until the recorder stops. Follows
/// the default output device when it changes and retries if it disappears.
pub(crate) fn run(shared: Arc<AudioShared>) -> Result<()> {
    let _com = MfThread::init()?;
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let input = AudioInput(shared.clone());
    while shared.is_running() {
        if capture_default_device(&enumerator, &shared, &input).is_err() {
            std::thread::sleep(RETRY_INTERVAL);
        }
    }
    Ok(())
}

/// Captures until the recorder stops or the default device changes.
fn capture_default_device(enumerator: &IMMDeviceEnumerator, shared: &AudioShared, input: &AudioInput) -> Result<()> {
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };
    let device_id = id_of(&device)?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &format(),
            None,
        )?;
    }
    let capture: IAudioCaptureClient = unsafe { client.GetService()? };
    unsafe { client.Start()? };

    let result = (|| -> Result<()> {
        let mut samples = Vec::new();
        let mut last_device_check = Instant::now();
        while shared.is_running() {
            std::thread::sleep(POLL_INTERVAL);
            while unsafe { capture.GetNextPacketSize()? } > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
                let count = frames as usize * CHANNELS;
                samples.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    samples.resize(count, 0.0);
                } else {
                    samples.extend_from_slice(unsafe { std::slice::from_raw_parts(data as *const f32, count) });
                }
                unsafe { capture.ReleaseBuffer(frames)? };
                input.push(DESKTOP_TRACK, &samples);
            }
            if last_device_check.elapsed() >= DEVICE_CHECK_INTERVAL {
                last_device_check = Instant::now();
                let current = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.and_then(|d| id_of(&d));
                if current.as_deref().ok() != Some(device_id.as_str()) {
                    return Ok(());
                }
            }
        }
        Ok(())
    })();
    unsafe {
        let _ = client.Stop();
    }
    result
}

fn id_of(device: &IMMDevice) -> Result<String> {
    let id = unsafe { device.GetId()? };
    let text = unsafe { id.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
    Ok(text)
}

/// 48 kHz stereo float, the format every clip track uses.
fn format() -> WAVEFORMATEX {
    let block_align = (CHANNELS * 4) as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}
