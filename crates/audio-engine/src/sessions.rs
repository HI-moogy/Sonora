use std::collections::HashSet;

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::process_tree::ProcessTree;

#[derive(Debug, Clone)]
pub struct AudioSession {
    pub process_id: u32,
    pub display_name: String,
    pub is_system_sounds: bool,
}

/// Calls `visit` with every audio session on every active output device.
/// Apps don't all play to the default device (some pick their own output,
/// or are routed to a virtual one), so looking at just the default misses
/// them. A device or session that can't be read is skipped.
pub(crate) fn visit_sessions(
    mut visit: impl FnMut(&IAudioSessionControl2),
) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for d in 0..devices.GetCount()? {
            let Ok(manager) = devices
                .Item(d)
                .and_then(|device| device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None))
            else {
                continue;
            };
            let Ok(list) = manager.GetSessionEnumerator() else {
                continue;
            };
            for s in 0..list.GetCount().unwrap_or(0) {
                if let Ok(control) = list
                    .GetSession(s)
                    .and_then(|c| c.cast::<IAudioSessionControl2>())
                {
                    visit(&control);
                }
            }
        }
    }
    Ok(())
}

/// The processes that own an audio session on any active output device.
pub(crate) fn session_owners() -> windows::core::Result<HashSet<u32>> {
    let mut owners = HashSet::new();
    visit_sessions(|control| {
        if let Ok(pid) = unsafe { control.GetProcessId() } {
            if pid != 0 {
                owners.insert(pid);
            }
        }
    })?;
    Ok(owners)
}

/// Every audio session on every active output device, i.e. every app that
/// has played or is playing audio. A helper process playing audio for an
/// app (see [`crate::process_tree`]) is listed as that app, not on its own.
/// A process playing to several devices appears once per device.
pub fn enumerate_sessions() -> windows::core::Result<Vec<AudioSession>> {
    let mut sessions = Vec::new();
    visit_sessions(|control2| unsafe {
        let process_id = control2.GetProcessId().unwrap_or(0);
        let is_system_sounds = control2.IsSystemSoundsSession() == S_OK;

        let session_name = match control2.GetDisplayName() {
            Ok(raw) if !raw.is_null() => {
                let name = raw.to_string().unwrap_or_default();
                CoTaskMemFree(Some(raw.0 as *const _));
                name
            }
            _ => String::new(),
        };
        // Most apps never set a session display name (WASAPI leaves it
        // to Volume Mixer to resolve from the process itself), so fall
        // back to the owning executable's filename.
        let display_name = if session_name.is_empty() {
            process_executable_name(process_id).unwrap_or_default()
        } else {
            session_name
        };

        sessions.push(AudioSession {
            process_id,
            display_name,
            is_system_sounds,
        });
    })?;
    if let Ok(tree) = ProcessTree::snapshot() {
        let owners: HashSet<u32> = sessions
            .iter()
            .map(|s| s.process_id)
            .filter(|&pid| pid != 0)
            .collect();
        let apps = tree.owning_apps(&owners);
        sessions.retain(|s| {
            apps.get(&s.process_id)
                .is_none_or(|&app| app == s.process_id)
        });
    }
    Ok(sessions)
}

/// Resolves a pid to its owning executable's filename (e.g. "steam.exe"),
/// for sessions WASAPI doesn't give a display name for. Returns `None` for
/// pid 0 (the system sounds session) or a process we can't query.
pub fn process_executable_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 260];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.ok()?;
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}
