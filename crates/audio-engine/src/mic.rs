use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};

use nnnoiseless::DenoiseState;
use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::com::ComGuard;
use crate::mixer::{SourceInner, MAX_BUFFERED_SAMPLES};

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

fn open_default_capture_client(
    format: &WAVEFORMATEX,
) -> Result<(IAudioClient, IAudioCaptureClient, HANDLE)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;
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

/// Runs entirely on its own thread: captures the default microphone, runs
/// it through RNNoise in RNNoise's native 10ms/480-sample mono frames, and
/// pushes the cleaned audio (duplicated to stereo) into `source.samples`
/// for the mixer to pick up, exactly like a process capture source.
///
/// RNNoise expects samples pre-scaled to 16-bit PCM range
/// (`[-32768.0, 32767.0]`), not the `[-1.0, 1.0]` range the rest of this
/// engine uses, so samples are scaled up before and back down after.
pub(crate) fn run_mic_capture_thread(source: Arc<SourceInner>, ready_tx: mpsc::Sender<Result<()>>) {
    let _com = match ComGuard::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let format = mono_format();
    let setup = open_default_capture_client(&format);
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

    let mut denoiser = DenoiseState::new();
    let mut staging: Vec<f32> = Vec::with_capacity(DenoiseState::FRAME_SIZE * 4);
    let mut in_frame = vec![0.0f32; DenoiseState::FRAME_SIZE];
    let mut out_frame = vec![0.0f32; DenoiseState::FRAME_SIZE];

    while !source.stop.load(Ordering::Relaxed) {
        let wait_result = unsafe { WaitForSingleObject(event, 100) };
        if wait_result != WAIT_OBJECT_0 {
            continue;
        }

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
            if num_frames > 0 && !data_ptr.is_null() {
                let samples =
                    unsafe { std::slice::from_raw_parts(data_ptr as *const f32, num_frames as usize) };
                staging.extend_from_slice(samples);
            }
            unsafe {
                let _ = capture_service.ReleaseBuffer(num_frames);
            }

            while staging.len() >= DenoiseState::FRAME_SIZE {
                for (dst, src) in in_frame.iter_mut().zip(staging.drain(..DenoiseState::FRAME_SIZE)) {
                    *dst = src * 32768.0;
                }
                denoiser.process_frame(&mut out_frame, &in_frame);

                let mut buffer = source.samples.lock().unwrap();
                let mut peak = 0.0f32;
                for &cleaned in out_frame.iter() {
                    let sample = (cleaned / 32768.0).clamp(-1.0, 1.0);
                    peak = peak.max(sample.abs());
                    buffer.push_back(sample); // left
                    buffer.push_back(sample); // right (mono duplicated to stereo)
                }
                while buffer.len() > MAX_BUFFERED_SAMPLES {
                    buffer.pop_front();
                }
                drop(buffer);
                source.level.store(peak);
            }
        }
    }

    unsafe {
        let _ = capture_client.Stop();
        let _ = CloseHandle(event);
    }
}
