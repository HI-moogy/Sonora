use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, Clone)]
pub struct AudioSession {
    pub process_id: u32,
    pub display_name: String,
    pub is_system_sounds: bool,
}

/// Enumerates the audio sessions currently active on the default render endpoint,
/// i.e. every process that has played or is playing audio through it.
pub fn enumerate_sessions() -> windows::core::Result<Vec<AudioSession>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let session_list = manager.GetSessionEnumerator()?;
        let count = session_list.GetCount()?;

        let mut sessions = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let control = session_list.GetSession(i)?;
            let control2: IAudioSessionControl2 = control.cast()?;

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
        }

        Ok(sessions)
    }
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
