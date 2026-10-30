//! An app's own output while Sonora routes it: turned down while the mixer
//! plays its captured copy, or (playing straight on its channel's device)
//! set to the channel's volume and pointed at that device.
//!
//! Windows' per-app (process-loopback) capture is taken *after* the app's
//! session volume and mute. Muting an app therefore silences its capture
//! too, but turning it down only scales the capture: measured, a session at
//! 0.01% captures at exactly 1/10,000 of full and boosting it 10,000×
//! restores the original. So apps routed through the mixer are turned down
//! to [`ROUTED_VOLUME`] (inaudible) and the mixer boosts their capture by
//! [`ROUTED_BOOST`].
//!
//! Windows remembers per-app volumes (and per-app output devices) across
//! restarts, so what each routed app had before is also kept on disk
//! ([`common::RoutedApps`]); if Sonora exits without restoring it (crash,
//! killed), [`recover`] puts it back.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use common::RoutedApps;
use windows::core::{Interface, Result};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{AudioSessionStateActive, IAudioSessionControl2, ISimpleAudioVolume};

use crate::app_output;
use crate::sessions::{process_executable_name, visit_sessions};

/// Session volume routed apps are turned down to: −80 dB, inaudible.
pub const ROUTED_VOLUME: f32 = 1e-4;
/// Gain that brings a routed app's capture back to its original level.
pub const ROUTED_BOOST: f32 = 1.0 / ROUTED_VOLUME;

/// The on-disk record, cached so the per-tick routing check doesn't read it.
static ROUTED_APPS: Mutex<Option<RoutedApps>> = Mutex::new(None);

fn with_routed_apps<R>(change: impl FnOnce(&mut RoutedApps) -> (R, bool)) -> R {
    let mut cache = ROUTED_APPS.lock().unwrap();
    let apps = cache.get_or_insert_with(common::load_routed_apps);
    let (result, changed) = change(apps);
    if changed {
        let _ = common::save_routed_apps(apps);
    }
    result
}

fn is_routed(volume: f32) -> bool {
    (volume - ROUTED_VOLUME).abs() <= ROUTED_VOLUME * 0.01
}

fn app_key(pid: u32) -> Option<String> {
    process_executable_name(pid).map(|name| name.to_lowercase())
}

/// One audio session of an app: the process that owns it, the output
/// device it plays on, and its controls.
pub struct OwnedSession {
    pub pid: u32,
    /// An id from the device lists.
    pub device: String,
    control: IAudioSessionControl2,
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
}

impl OwnedSession {
    /// Whether it's playing now (apps keep sessions they no longer play on,
    /// like the one on a device they've moved from).
    pub fn is_playing(&self) -> bool {
        unsafe { self.control.GetState() }.is_ok_and(|state| state == AudioSessionStateActive)
    }

    /// The loudest it has played since last asked, as Windows meters it
    /// (for apps Sonora can't capture).
    pub fn peak(&self) -> f32 {
        self.meter.as_ref().and_then(|meter| unsafe { meter.GetPeakValue() }.ok()).unwrap_or(0.0)
    }

    fn volume(&self) -> Option<f32> {
        unsafe { self.volume.GetMasterVolume() }.ok()
    }

    fn set_volume(&self, volume: f32) -> bool {
        unsafe { self.volume.SetMasterVolume(volume, std::ptr::null()) }.is_ok()
    }
}

/// Every audio session on every active output device.
pub fn all_sessions() -> Result<Vec<OwnedSession>> {
    sessions(None)
}

/// Every audio session owned by a process in `pids` (or by any process, for
/// `None`), on every active output device.
fn sessions(pids: Option<&HashSet<u32>>) -> Result<Vec<OwnedSession>> {
    let mut found = Vec::new();
    visit_sessions(|device, control| {
        let Ok(pid) = (unsafe { control.GetProcessId() }) else {
            return;
        };
        if pid != 0 && pids.is_none_or(|pids| pids.contains(&pid)) {
            if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                let meter = control.cast::<IAudioMeterInformation>().ok();
                found.push(OwnedSession { pid, device: device.to_string(), control: control.clone(), volume, meter });
            }
        }
    })?;
    Ok(found)
}

/// Turns every session of the processes in `pids` (an app's audio
/// processes) down to [`ROUTED_VOLUME`]; see [`record_originals`] and
/// [`enforce`].
pub fn route(pids: &HashSet<u32>) -> bool {
    let Ok(sessions) = sessions(Some(pids)) else {
        return false;
    };
    record_originals(&sessions);
    enforce(&sessions)
}

/// Records the volume each process owning one of `sessions` has before it
/// is turned down, once per executable, so [`unroute`] and [`recover`] can
/// put it back. Call whenever an app's sessions are (re)found, before
/// [`enforce`] or [`set_volume`].
pub fn record_originals(sessions: &[OwnedSession]) {
    let pids: HashSet<u32> = sessions.iter().map(|session| session.pid).collect();
    for pid in pids {
        let Some(key) = app_key(pid) else { continue };
        with_routed_apps(|apps| {
            if apps.original_volumes.contains_key(&key) {
                return ((), false);
            }
            // A session already at the routed level was left that way by
            // an earlier run; its real original is unknown, so use full.
            let original = sessions
                .iter()
                .filter(|session| session.pid == pid)
                .filter_map(OwnedSession::volume)
                .find(|&v| !is_routed(v))
                .unwrap_or(1.0);
            apps.original_volumes.insert(key, original);
            ((), true)
        });
    }
}

/// Turns every session in `sessions` down to [`ROUTED_VOLUME`] (cheap: only
/// sessions not already there are changed). Returns whether every one now
/// reads back at [`ROUTED_VOLUME`], i.e. whether the app's capture can
/// safely be boosted; false if there are none.
pub fn enforce(sessions: &[OwnedSession]) -> bool {
    let mut all_routed = !sessions.is_empty();
    for session in sessions {
        if !session.volume().is_some_and(is_routed) {
            session.set_volume(ROUTED_VOLUME);
        }
        all_routed &= session.volume().is_some_and(is_routed);
    }
    all_routed
}

/// Sets every session in `sessions` to `volume`, for an app playing
/// straight on its channel's device (cheap: only sessions not already
/// there are changed).
pub fn set_volume(sessions: &[OwnedSession], volume: f32) {
    for session in sessions {
        if session.volume().is_none_or(|current| (current - volume).abs() > 1e-4) {
            session.set_volume(volume);
        }
    }
}

/// Records whether the apps owning `sessions` play straight on their
/// channel's device (`direct`: their volume is the channel's) or through
/// the mixer (turned down). Only the latter are left near-silent if Sonora
/// stops without restoring them.
pub fn mark_direct(sessions: &[OwnedSession], direct: bool) {
    let keys: HashSet<String> = sessions.iter().filter_map(|session| app_key(session.pid)).collect();
    with_routed_apps(|apps| {
        let before = apps.direct.len();
        if direct {
            apps.direct.extend(keys);
        } else {
            apps.direct.retain(|key| !keys.contains(key));
        }
        ((), apps.direct.len() != before)
    });
}

/// Points the processes in `pids` (an app's audio processes, each with a
/// session) at output `device_id` with Windows' per-app output setting,
/// remembering what each app was set to before; or, with `None`, puts that
/// back. An app's record is dropped only once it's put back.
pub fn point(pids: &HashSet<u32>, device_id: Option<&str>) {
    let keys: HashMap<u32, String> = pids.iter().filter_map(|&pid| Some((pid, app_key(pid)?))).collect();
    match device_id {
        Some(device_id) => {
            for (&pid, key) in &keys {
                with_routed_apps(|apps| {
                    if apps.pointed.contains_key(key) {
                        return ((), false);
                    }
                    match app_output::get(pid) {
                        Ok(previous) => {
                            apps.pointed.insert(key.clone(), previous);
                            ((), true)
                        }
                        Err(_) => ((), false),
                    }
                });
                // Only once the previous choice is on record.
                if with_routed_apps(|apps| (apps.pointed.contains_key(key), false)) {
                    let _ = app_output::set(pid, Some(device_id));
                }
            }
        }
        None => {
            let previous: HashMap<String, Option<String>> = with_routed_apps(|apps| {
                let previous =
                    keys.values().filter_map(|key| Some((key.clone(), apps.pointed.get(key)?.clone()))).collect();
                (previous, false)
            });
            let mut restored = HashSet::new();
            for (&pid, key) in &keys {
                if let Some(device) = previous.get(key) {
                    if app_output::set(pid, device.as_deref()).is_ok() {
                        restored.insert(key.clone());
                    }
                }
            }
            if !restored.is_empty() {
                with_routed_apps(|apps| {
                    apps.pointed.retain(|key, _| !restored.contains(key));
                    ((), true)
                });
            }
        }
    }
}

/// Gives the processes in `pids` back their own output: the volume each had
/// before Sonora routed it, and the output device. An app's record is only
/// dropped once one of its sessions is back up: one that couldn't be
/// reached (the session list failed, or the app had closed) keeps it, so
/// [`recover`] and the quit warning can still put it back later.
pub fn unroute(pids: &HashSet<u32>) {
    point(pids, None);
    let keys: HashMap<u32, String> = pids
        .iter()
        .filter_map(|&pid| Some((pid, app_key(pid)?)))
        .collect();
    let originals: HashMap<String, f32> = with_routed_apps(|apps| {
        // Several processes can share an executable, and so a record.
        let originals = keys
            .values()
            .filter_map(|key| Some((key.clone(), *apps.original_volumes.get(key)?)))
            .collect();
        (originals, false)
    });
    let Ok(sessions) = sessions(Some(pids)) else { return };
    let mut restored = HashSet::new();
    for session in sessions {
        let key = keys.get(&session.pid);
        let original = key.and_then(|key| originals.get(key)).copied().unwrap_or(1.0);
        if session.set_volume(original) {
            restored.extend(key.cloned());
        }
    }
    if !restored.is_empty() {
        with_routed_apps(|apps| {
            apps.original_volumes.retain(|key, _| !restored.contains(key));
            apps.direct.retain(|key| !restored.contains(key));
            ((), true)
        });
    }
}

/// Restores apps an earlier run left routed (it crashed or was killed):
/// turned down, set to a channel's volume, or pointed at a channel's
/// device. Apps in `routed_now` are being routed and are left alone; apps
/// that aren't running keep their record until they are.
pub fn recover(routed_now: &[u32]) {
    let (originals, direct, pointed) = with_routed_apps(|apps| {
        ((apps.original_volumes.clone(), apps.direct.clone(), apps.pointed.clone()), false)
    });
    if originals.is_empty() && pointed.is_empty() {
        return;
    }
    let active: HashSet<String> = routed_now.iter().filter_map(|&pid| app_key(pid)).collect();
    let Ok(sessions) = sessions(None) else { return };
    let (mut restored, mut unpointed) = (HashSet::new(), HashSet::new());
    for session in sessions {
        let Some(key) = app_key(session.pid) else { continue };
        if active.contains(&key) {
            continue;
        }
        if let Some(&original) = originals.get(&key) {
            // One left turned down is put back; so is one whose volume
            // Sonora set to its channel's.
            if direct.contains(&key) || session.volume().is_some_and(is_routed) {
                session.set_volume(original);
            }
            restored.insert(key.clone());
        }
        if let Some(previous) = pointed.get(&key) {
            if app_output::set(session.pid, previous.as_deref()).is_ok() {
                unpointed.insert(key);
            }
        }
    }
    if !restored.is_empty() || !unpointed.is_empty() {
        with_routed_apps(|apps| {
            apps.original_volumes.retain(|key, _| !restored.contains(key));
            apps.direct.retain(|key| !restored.contains(key));
            apps.pointed.retain(|key, _| !unpointed.contains(key));
            ((), true)
        });
    }
}

/// Apps Sonora turned down that closed before it could turn them back
/// up (executable names). Windows remembers an app's volume and only lets
/// it be changed while the app is playing, so these start almost silent
/// until Sonora runs while they're open and [`recover`] fixes them. Apps
/// that played straight on their device were never turned down.
pub fn turned_down_apps_not_running() -> Vec<String> {
    let recorded: Vec<String> = with_routed_apps(|apps| {
        let turned_down = apps.original_volumes.keys().filter(|key| !apps.direct.contains(*key)).cloned().collect();
        (turned_down, false)
    });
    if recorded.is_empty() {
        return recorded;
    }
    let Ok(running) = crate::process_tree::running_executables() else {
        return Vec::new();
    };
    let mut left: Vec<String> = recorded
        .into_iter()
        .filter(|app| !running.contains(app))
        .collect();
    left.sort();
    left
}

/// Whether any app's original volume or output is still recorded (cheap:
/// no I/O).
pub fn has_pending_routes() -> bool {
    with_routed_apps(|apps| (!apps.original_volumes.is_empty() || !apps.pointed.is_empty(), false))
}
