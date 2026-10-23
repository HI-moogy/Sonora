//! Silencing an app's own output while the mixer plays its captured copy.
//!
//! Windows' per-app (process-loopback) capture is taken *after* the app's
//! session volume and mute. Muting an app therefore silences its capture
//! too, but turning it down only scales the capture: measured, a session at
//! 0.01% captures at exactly 1/10,000 of full and boosting it 10,000×
//! restores the original. So routed apps are turned down to
//! [`ROUTED_VOLUME`] (inaudible) and the mixer boosts their capture by
//! [`ROUTED_BOOST`].
//!
//! Windows remembers per-app volumes across restarts, so each routed app's
//! original volume is also kept on disk ([`common::RoutedApps`]); if Sonora
//! exits without restoring it (crash, killed), [`recover`] puts it back.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use common::RoutedApps;
use windows::core::{Interface, Result};
use windows::Win32::Media::Audio::ISimpleAudioVolume;

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

/// An audio session's volume control, with the pid of the process that
/// owns the session.
pub type OwnedSession = (u32, ISimpleAudioVolume);

/// Every audio session on every active output device.
pub fn all_sessions() -> Result<Vec<OwnedSession>> {
    sessions(None)
}

/// Every audio session owned by a process in `pids` (or by any process, for
/// `None`), on every active output device.
fn sessions(pids: Option<&HashSet<u32>>) -> Result<Vec<OwnedSession>> {
    let mut found = Vec::new();
    visit_sessions(|control| {
        let Ok(pid) = (unsafe { control.GetProcessId() }) else {
            return;
        };
        if pid != 0 && pids.is_none_or(|pids| pids.contains(&pid)) {
            if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                found.push((pid, volume));
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
/// [`enforce`].
pub fn record_originals(sessions: &[OwnedSession]) {
    let pids: HashSet<u32> = sessions.iter().map(|&(pid, _)| pid).collect();
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
                .filter(|(owner, _)| *owner == pid)
                .filter_map(|(_, v)| unsafe { v.GetMasterVolume() }.ok())
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
    for (_, volume) in sessions {
        unsafe {
            if !volume.GetMasterVolume().is_ok_and(is_routed) {
                let _ = volume.SetMasterVolume(ROUTED_VOLUME, std::ptr::null());
            }
            all_routed &= volume.GetMasterVolume().is_ok_and(is_routed);
        }
    }
    all_routed
}

/// Puts the sessions of the processes in `pids` back to the volume each
/// had before [`route`].
pub fn unroute(pids: &HashSet<u32>) {
    let keys: HashMap<u32, String> = pids
        .iter()
        .filter_map(|&pid| Some((pid, app_key(pid)?)))
        .collect();
    // Several processes can share an executable, and so a record.
    let originals: HashMap<String, f32> = with_routed_apps(|apps| {
        let originals = keys
            .values()
            .filter_map(|key| Some((key.clone(), *apps.original_volumes.get(key)?)))
            .collect();
        apps.original_volumes
            .retain(|key, _| !keys.values().any(|k| k == key));
        (originals, true)
    });
    if let Ok(sessions) = sessions(Some(pids)) {
        for (pid, volume) in sessions {
            unsafe {
                let original = keys
                    .get(&pid)
                    .and_then(|key| originals.get(key))
                    .copied()
                    .unwrap_or(1.0);
                let _ = volume.SetMasterVolume(original, std::ptr::null());
            }
        }
    }
}

/// Restores apps an earlier run left turned down (it crashed or was
/// killed). Apps in `routed_now` are being routed and are left alone; apps
/// that aren't running keep their record until they are.
pub fn recover(routed_now: &[u32]) {
    let pending = with_routed_apps(|apps| (apps.original_volumes.clone(), false));
    if pending.is_empty() {
        return;
    }
    let active: HashSet<String> = routed_now.iter().filter_map(|&pid| app_key(pid)).collect();
    let Ok(sessions) = sessions(None) else { return };
    let mut restored = HashSet::new();
    for (pid, volume) in sessions {
        let Some(key) = app_key(pid) else { continue };
        let Some(&original) = pending.get(&key) else {
            continue;
        };
        if active.contains(&key) {
            continue;
        }
        unsafe {
            if volume.GetMasterVolume().is_ok_and(is_routed) {
                let _ = volume.SetMasterVolume(original, std::ptr::null());
            }
        }
        restored.insert(key);
    }
    if !restored.is_empty() {
        with_routed_apps(|apps| {
            apps.original_volumes
                .retain(|key, _| !restored.contains(key));
            ((), true)
        });
    }
}

/// Apps Sonora turned down that closed before it could turn them back
/// up (executable names). Windows remembers an app's volume and only lets
/// it be changed while the app is playing, so these start almost silent
/// until Sonora runs while they're open and [`recover`] fixes them.
pub fn turned_down_apps_not_running() -> Vec<String> {
    let recorded: Vec<String> =
        with_routed_apps(|apps| (apps.original_volumes.keys().cloned().collect(), false));
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

/// Whether any app's original volume is still recorded (cheap: no I/O).
pub fn has_pending_routes() -> bool {
    with_routed_apps(|apps| (!apps.original_volumes.is_empty(), false))
}
