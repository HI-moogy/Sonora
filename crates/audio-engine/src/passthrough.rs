use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::loopback::{activate_process_loopback_client, standard_format};
use crate::render::open_default_render_client;

const BUFFER_DURATION_100NS: i64 = 2_000_000;
/// How long each WaitForSingleObject call blocks before rechecking the
/// deadline; keeps the loop responsive to `duration` expiring even if the
/// source goes quiet and never signals.
const POLL_TIMEOUT_MS: u32 = 100;

/// M1 proof-of-concept: capture everything `target_process_id` plays and
/// render it straight through to the default output device, unmodified,
/// for `duration`. Proves the capture -> render pipeline end to end before
/// the real mixer (per-source volume, multiple buses) replaces this loop.
///
/// Mirrors Microsoft's ApplicationLoopback sample's Initialize flags and
/// event-driven buffering exactly: this activation (a virtual,
/// process-scoped capture endpoint rather than a normal device) has
/// documented reliability issues when driven differently, including from
/// other callers polling it instead of waiting on its buffer-ready event.
pub fn run_passthrough(target_process_id: u32, duration: Duration) -> Result<()> {
    let format = standard_format();

    let capture_client = activate_process_loopback_client(target_process_id)?;
    let capture_event = unsafe { CreateEventW(None, false, false, None)? };
    unsafe {
        capture_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            BUFFER_DURATION_100NS,
            0,
            &format,
            None,
        )?;
        capture_client.SetEventHandle(capture_event)?;
    }
    let capture_service: IAudioCaptureClient = unsafe { capture_client.GetService()? };

    let (render_client, render_service) = open_default_render_client(&format)?;

    unsafe {
        capture_client.Start()?;
        render_client.Start()?;
    }

    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let wait_result = unsafe { WaitForSingleObject(capture_event, POLL_TIMEOUT_MS) };
        if wait_result != WAIT_OBJECT_0 {
            continue; // timed out this slice with nothing signaled; recheck the deadline
        }

        loop {
            let packet_size = unsafe { capture_service.GetNextPacketSize()? };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            unsafe {
                capture_service.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)?;
            }

            if num_frames > 0 {
                let render_ptr = unsafe { render_service.GetBuffer(num_frames)? };
                let bytes = num_frames as usize * format.nBlockAlign as usize;
                unsafe {
                    std::ptr::copy_nonoverlapping(data_ptr, render_ptr, bytes);
                    render_service.ReleaseBuffer(num_frames, 0)?;
                }
            }
            unsafe {
                capture_service.ReleaseBuffer(num_frames)?;
            }
        }
    }

    unsafe {
        capture_client.Stop()?;
        render_client.Stop()?;
        CloseHandle(capture_event)?;
    }
    Ok(())
}
