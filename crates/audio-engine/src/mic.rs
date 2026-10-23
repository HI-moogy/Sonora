use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::com::ComGuard;
use crate::denoise::{MicCleanup, MicPipeline};
use crate::mixer::SourceInner;

const CAPTURE_BUFFER_DURATION_100NS: i64 = 2_000_000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

fn mono_format() -> WAVEFORMATEX {
    let sample_rate = 48_000u32;
    let bits_per_sample = 32u16;
    let block_align = bits_per_sample / 8;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: 1,
        nSamplesPerSec: sample_rate,
        nAvgBytesPerSec: sample_rate * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits_per_sample,
        cbSize: 0,
    }
}

/// Opens `device_id` (from [`crate::devices::list_input_devices`]), or the
/// system default microphone when `None`.
fn open_capture_client(
    device_id: Option<&str>,
    format: &WAVEFORMATEX,
) -> Result<(IAudioClient, IAudioCaptureClient, HANDLE)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = match device_id {
            Some(id) => {
                let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))?
            }
            None => enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?,
        };
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let event = CreateEventW(None, false, false, None)?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            CAPTURE_BUFFER_DURATION_100NS,
            0,
            format,
            None,
        )?;
        client.SetEventHandle(event)?;
        let capture_service: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok((client, capture_service, event))
    }
}

/// Runs entirely on its own thread: captures the microphone and hands it to
/// a [`MicPipeline`], which cleans it up as `cleanup` says and queues it for
/// the mixer like an app's audio.
pub(crate) fn run_mic_capture_thread(
    source: Arc<SourceInner>,
    device_id: Option<String>,
    cleanup: Arc<MicCleanup>,
    ready_tx: mpsc::Sender<Result<()>>,
) {
    let _com = match ComGuard::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let format = mono_format();
    let setup = open_capture_client(device_id.as_deref(), &format);
    let (capture_client, capture_service, event) = match setup {
        Ok(triple) => {
            let _ = ready_tx.send(Ok(()));
            triple
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let mut pipeline = MicPipeline::new(cleanup);

    while !source.stop.load(Ordering::Relaxed) {
        let wait_result = unsafe { WaitForSingleObject(event, 100) };
        if wait_result != WAIT_OBJECT_0 {
            continue;
        }
        let discard = pipeline.begin_batch();

        loop {
            let packet_size = match unsafe { capture_service.GetNextPacketSize() } {
                Ok(size) => size,
                Err(_) => break,
            };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe { capture_service.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None) }
                .is_err()
            {
                break;
            }
            if num_frames > 0 && !data_ptr.is_null() && !discard {
                let samples =
                    unsafe { std::slice::from_raw_parts(data_ptr as *const f32, num_frames as usize) };
                pipeline.push(samples, &source);
            }
            unsafe {
                let _ = capture_service.ReleaseBuffer(num_frames);
            }
        }
    }

    unsafe {
        let _ = capture_client.Stop();
        let _ = CloseHandle(event);
    }
}
