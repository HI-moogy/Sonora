//! Capturing one app's audio on Windows: process loopback of the app (and
//! its helper processes). Routed through the mixer, the app's own sessions
//! are turned down so only the mixer's copy is heard (see
//! [`crate::session_volume`]). When nothing needs the mixer, the app is
//! instead pointed at its channel's device and, once Windows shows it
//! playing there, plays direct: its sessions get the channel's volume and
//! the capture only feeds meters and clips (see [`crate::mixer::RoutePlan`]).

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
use crate::devices::default_output_device_id;
use crate::loopback::{activate_process_loopback_client, per_app_capture_supported, standard_format};
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
/// How often an app Sonora can't capture is metered from Windows' own
/// meter.
const UNCAPTURED_METER_INTERVAL: Duration = Duration::from_millis(20);
/// How often an app pointed at its channel's device is looked up again
/// while it hasn't moved there yet: its new session there is how Sonora
/// sees that it has.
const MOVE_RESCAN_INTERVAL: Duration = Duration::from_secs(1);

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
    let owners: HashSet<u32> = all.iter().map(|session| session.pid).collect();
    let (processes, target) = match ProcessTree::snapshot() {
        Ok(tree) => (
            tree.app_audio_processes(pid, &owners),
            tree.capture_target(pid, &owners),
        ),
        Err(_) => (HashSet::from([pid]), pid),
    };
    let sessions = all
        .into_iter()
        .filter(|session| processes.contains(&session.pid))
        .collect();
    AppAudio {
        processes,
        target,
        sessions,
    }
}

/// Whether the app can play direct on `device` (`None`: Windows' default
/// output), as its sessions show: while `direct`, it stays so unless it
/// plays on another device; otherwise it becomes so once it plays on that
/// device and nowhere else. An app that's gone quiet keeps its route.
fn plays_on(sessions: &[OwnedSession], device: Option<&str>, direct: bool) -> bool {
    let target = match device {
        Some(id) => id.to_string(),
        None => match default_output_device_id() {
            Ok(id) => id,
            Err(_) => return false,
        },
    };
    let playing: Vec<&OwnedSession> = sessions.iter().filter(|session| session.is_playing()).collect();
    let elsewhere = playing.iter().any(|session| session.device != target);
    let there = playing.iter().any(|session| session.device == target);
    !elsewhere && (direct || there)
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
    // Without per-app capture (Windows 10) the app can only play direct:
    // nothing is captured, and it's never turned down.
    let mut capture = if per_app_capture_supported() {
        match LoopbackCapture::start(app.target) {
            Ok(capture) => Some(capture),
            Err(error) => {
                let _ = ready_tx.send(Err(error));
                return;
            }
        }
    } else {
        None
    };
    let _ = ready_tx.send(Ok(()));
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
    // The device Sonora last pointed the app's processes at (`Some(None)`:
    // gave them back their own), so it's only done when that changes.
    let mut pointed: Option<Option<String>> = None;
    // Playing straight on its channel's device (see `crate::mixer::RoutePlan`).
    let mut direct = false;

    while !source.stop.load(Ordering::Relaxed) {
        if found.is_none() && Instant::now() >= next_scan {
            found = Some(app_audio(pid));
        }
        if let Some(app) = found.take() {
            // Keep the old capture if the new one can't start.
            if capture.as_ref().is_some_and(|capture| app.target != capture.pid) {
                if let Ok(new_capture) = LoopbackCapture::start(app.target) {
                    capture = Some(new_capture);
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
            // New processes are pointed too.
            if *routed != app.processes {
                pointed = None;
            }
            *routed = app.processes;
            sessions = app.sessions;
            last_scan = Instant::now();
            next_scan = last_scan + APP_RESCAN_INTERVAL;
            last_routed = Instant::now() - ROUTE_ENFORCE_INTERVAL;
        }
        if last_routed.elapsed() >= ROUTE_ENFORCE_INTERVAL {
            let routed = source.routed.lock().unwrap();
            if source.stop.load(Ordering::Relaxed) {
                break;
            }
            let plan = source.plan.lock().unwrap().clone();
            // Point the app at its channel's device, or give it back its own.
            let wanted = if plan.direct { plan.device.clone() } else { None };
            if pointed.as_ref() != Some(&wanted) {
                session_volume::point(&routed, wanted.as_deref());
                pointed = Some(wanted);
            }
            // Uncaptured, it's under the channel's volume wherever it plays:
            // it moves when it opens its audio again.
            let now_direct =
                plan.direct && (capture.is_none() || plays_on(&sessions, plan.device.as_deref(), direct));
            if now_direct != direct {
                direct = now_direct;
                session_volume::mark_direct(&sessions, direct);
                // What's queued was captured the other way.
                source.samples.lock().unwrap().clear();
                source.direct.store(direct, Ordering::Relaxed);
                if !direct && capture.is_none() {
                    // Nothing could play it turned down: give it back its own.
                    session_volume::unroute(&routed);
                }
            }
            if direct {
                session_volume::set_volume(&sessions, plan.volume);
                all_routed = false;
            } else if capture.is_some() {
                all_routed = session_volume::enforce(&sessions);
                if plan.direct {
                    // Pointed there: look for it playing there sooner.
                    next_scan = next_scan.min(last_scan + MOVE_RESCAN_INTERVAL);
                }
            }
            last_routed = Instant::now();
        }

        let Some(capture) = capture.as_ref() else {
            // Nothing captured: Windows' own meter shows it playing.
            std::thread::sleep(UNCAPTURED_METER_INTERVAL);
            if direct {
                source.report_level(sessions.iter().map(OwnedSession::peak).fold(0.0, f32::max));
            }
            continue;
        };
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
                // A direct app's audio is at its own volume: nothing to boost.
                let gain = if direct {
                    1.0
                } else {
                    let raw_peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                    let turned_down = raw_peak <= ROUTED_VOLUME * ROUTED_HEADROOM;
                    if all_routed && !turned_down {
                        // Louder than a turned-down app can be: some of its
                        // audio isn't turned down, likely a session we
                        // haven't found yet. Don't boost; look for it soon.
                        boost_blocked_until = Instant::now() + BOOST_BLOCK;
                        next_scan = next_scan.min(last_scan + APP_RESCAN_SOON);
                    }
                    if all_routed && turned_down && Instant::now() >= boost_blocked_until {
                        ROUTED_BOOST
                    } else {
                        1.0
                    }
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
