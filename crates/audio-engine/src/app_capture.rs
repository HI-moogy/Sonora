//! Capturing one app's audio on Windows: process loopback of the app (and
//! its helper processes), with the app's own sessions turned down so only
//! the mixer's copy is heard (see [`crate::session_volume`]).

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::com::{ComGuard, ProAudioThread};
use crate::loopback::{activate_process_loopback_client, standard_format};
use crate::mixer::{SourceInner, CHANNELS, MAX_BUFFERED_SAMPLES};
use crate::process_tree::ProcessTree;
use crate::session_volume::{self, OwnedSession, ROUTED_BOOST, ROUTED_VOLUME};

const CAPTURE_BUFFER_DURATION_100NS: i64 = 2_000_000;
/// How often a routed app's known sessions are checked and turned down
/// again if something (the app, the Windows mixer) turned them back up.
/// Cheap: reads each known session's volume.
const ROUTE_ENFORCE_INTERVAL: Duration = Duration::from_millis(100);
/// How often an app's audio processes and sessions are looked up again,
/// since apps create sessions and helper processes as they go (see
/// [`crate::process_tree`]). Costly: walks every session on every device
/// and the process list (~17 ms measured), so it runs far less often
/// than enforcement.
const APP_RESCAN_INTERVAL: Duration = Duration::from_secs(3);
/// How soon after the last look-up to look again when the captured audio
/// shows a session that isn't turned down yet (e.g. a new browser tab).
const APP_RESCAN_SOON: Duration = Duration::from_millis(500);
/// A turned-down app can't produce samples above `ROUTED_VOLUME` times its
/// own peak; apps with hot float output can exceed full scale a little, so
/// allow this much before treating a packet as not turned down.
const ROUTED_HEADROOM: f32 = 2.0;
/// After capturing audio that wasn't turned down despite every session
/// reading back as turned down (audio we can't reach, e.g. a child process),
/// don't boost again for this long.
const BOOST_BLOCK: Duration = Duration::from_secs(2);

/// A started process-loopback capture of one process (and its descendants).
struct LoopbackCapture {
    pid: u32,
    client: IAudioClient,
    service: IAudioCaptureClient,
    event: HANDLE,
}

impl LoopbackCapture {
    fn start(pid: u32) -> Result<Self> {
        let client = activate_process_loopback_client(pid)?;
        let event = unsafe { CreateEventW(None, false, false, None)? };
        let started = (|| -> Result<IAudioCaptureClient> {
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    CAPTURE_BUFFER_DURATION_100NS,
                    0,
                    &standard_format(),
                    None,
                )?;
                client.SetEventHandle(event)?;
                let service: IAudioCaptureClient = client.GetService()?;
                client.Start()?;
                Ok(service)
            }
        })();
        match started {
            Ok(service) => Ok(Self {
                pid,
                client,
                service,
                event,
            }),
            Err(error) => {
                unsafe {
                    let _ = CloseHandle(event);
                }
                Err(error)
            }
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// What makes up an app's audio right now (see [`crate::process_tree`]).
struct AppAudio {
    /// The app and its helper processes that own audio sessions.
    processes: HashSet<u32>,
    /// The process to capture the app's audio from.
    target: u32,
    /// Every audio session of `processes`.
    sessions: Vec<OwnedSession>,
}

/// Looks up app `pid`'s audio in one pass over the sessions. Falls back to
/// just `pid` if the process list can't be read.
fn app_audio(pid: u32) -> AppAudio {
    let all = session_volume::all_sessions().unwrap_or_default();
    let owners: HashSet<u32> = all.iter().map(|&(owner, _)| owner).collect();
    let (processes, target) = match ProcessTree::snapshot() {
        Ok(tree) => (
            tree.app_audio_processes(pid, &owners),
            tree.capture_target(pid, &owners),
        ),
        Err(_) => (HashSet::from([pid]), pid),
    };
    let sessions = all
        .into_iter()
        .filter(|(owner, _)| processes.contains(owner))
        .collect();
    AppAudio {
        processes,
        target,
        sessions,
    }
}

/// Runs entirely on its own OS thread: initializes this thread's own COM
/// apartment, starts capturing app `pid`'s audio, reports the outcome over
/// `ready_tx`, then (on success) captures until `source.stop` is set. COM
/// interface pointers are never moved across threads — each thread that
/// uses them creates and owns them itself.
pub(crate) fn run_capture_thread(pid: u32, source: Arc<SourceInner>, ready_tx: mpsc::Sender<Result<()>>) {
    let _com = match ComGuard::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let _pro_audio = ProAudioThread::join();

    let app = app_audio(pid);
    let mut capture = match LoopbackCapture::start(app.target) {
        Ok(capture) => {
            let _ = ready_tx.send(Ok(()));
            capture
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let mut found = Some(app);
    let mut sessions = Vec::new();
    let mut last_scan = Instant::now();
    let mut next_scan = last_scan + APP_RESCAN_INTERVAL;

    // Apps create new sessions as they go, which start at full volume, so
    // keep turning every session down. The capture is only boosted while
    // every session reads back as turned down; boosting audio that isn't
    // would make it 10,000x too loud.
    let mut last_routed = Instant::now() - ROUTE_ENFORCE_INTERVAL;
    let mut all_routed = false;
    let mut boost_blocked_until = Instant::now();
    let mut boosted = Vec::new();

    while !source.stop.load(Ordering::Relaxed) {
        if found.is_none() && Instant::now() >= next_scan {
            found = Some(app_audio(pid));
        }
        if let Some(app) = found.take() {
            // Keep the old capture if the new one can't start.
            if app.target != capture.pid {
                if let Ok(new_capture) = LoopbackCapture::start(app.target) {
                    capture = new_capture;
                }
            }
            let mut routed = source.routed.lock().unwrap();
            if source.stop.load(Ordering::Relaxed) {
                break;
            }
            // Give back processes that stopped being part of the app.
            let gone: HashSet<u32> = routed.difference(&app.processes).copied().collect();
            if !gone.is_empty() {
                session_volume::unroute(&gone);
            }
            session_volume::record_originals(&app.sessions);
            *routed = app.processes;
            sessions = app.sessions;
            last_scan = Instant::now();
            next_scan = last_scan + APP_RESCAN_INTERVAL;
            last_routed = Instant::now() - ROUTE_ENFORCE_INTERVAL;
        }
        if last_routed.elapsed() >= ROUTE_ENFORCE_INTERVAL {
            let _routed = source.routed.lock().unwrap();
            if source.stop.load(Ordering::Relaxed) {
                break;
            }
            all_routed = session_volume::enforce(&sessions);
            last_routed = Instant::now();
        }

        let wait_result = unsafe { WaitForSingleObject(capture.event, 100) };
        if wait_result != WAIT_OBJECT_0 {
            continue;
        }

        loop {
            let packet_size = match unsafe { capture.service.GetNextPacketSize() } {
                Ok(size) => size,
                Err(_) => break,
            };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe {
                capture
                    .service
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
            }
            .is_err()
            {
                break;
            }

            if num_frames > 0 && !data_ptr.is_null() {
                let samples = unsafe {
                    std::slice::from_raw_parts(
                        data_ptr as *const f32,
                        num_frames as usize * CHANNELS,
                    )
                };
                let raw_peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                let turned_down = raw_peak <= ROUTED_VOLUME * ROUTED_HEADROOM;
                if all_routed && !turned_down {
                    // Louder than a turned-down app can be: some of its audio
                    // isn't turned down, likely a session we haven't found
                    // yet. Don't boost; look for it soon.
                    boost_blocked_until = Instant::now() + BOOST_BLOCK;
                    next_scan = next_scan.min(last_scan + APP_RESCAN_SOON);
                }
                let gain = if all_routed && turned_down && Instant::now() >= boost_blocked_until {
                    ROUTED_BOOST
                } else {
                    1.0
                };
                boosted.clear();
                boosted.extend(samples.iter().map(|&s| s * gain));

                let mut buffer = source.samples.lock().unwrap();
                buffer.extend(boosted.iter().copied());
                while buffer.len() > MAX_BUFFERED_SAMPLES {
                    buffer.pop_front();
                }
            }
            unsafe {
                let _ = capture.service.ReleaseBuffer(num_frames);
            }
        }
    }
}
