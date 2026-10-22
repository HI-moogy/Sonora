use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// Windows' process-loopback capture is a non-destructive tap: capturing a
/// process's audio does not stop that process from also playing it through
/// its own, real audio session exactly as before. Controlling what the
/// user actually hears for a captured app therefore requires separately
/// controlling that app's own session here — the same mechanism Windows'
/// own Volume Mixer uses — not just adjusting our own captured copy.
fn find_session_volume(pid: u32) -> windows::core::Result<Option<ISimpleAudioVolume>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let session_list = manager.GetSessionEnumerator()?;
        let count = session_list.GetCount()?;

        for i in 0..count {
            let control = session_list.GetSession(i)?;
            let control2: IAudioSessionControl2 = control.cast()?;
            if control2.GetProcessId().unwrap_or(0) == pid {
                return Ok(Some(control2.cast()?));
            }
        }
        Ok(None)
    }
}

/// Mutes or unmutes `pid`'s own Windows audio session directly (not our
/// captured copy of it). Silently does nothing if the process currently
/// has no active session (e.g. not playing anything right now) rather than
/// erroring, since that's a normal, transient state.
pub fn set_session_muted(pid: u32, muted: bool) {
    match find_session_volume(pid) {
        Ok(Some(volume)) => unsafe {
            let _ = volume.SetMute(muted, std::ptr::null());
        },
        Ok(None) => {}
        Err(error) => eprintln!("session_volume: failed to look up session for pid {pid}: {error:?}"),
    }
}

/// Sets `pid`'s own Windows session volume directly, in `[0.0, 1.0]`
/// (Windows sessions can't be boosted past 100% this way, unlike our
/// internal mixer gain).
pub fn set_session_volume(pid: u32, level: f32) {
    match find_session_volume(pid) {
        Ok(Some(volume)) => unsafe {
            let _ = volume.SetMasterVolume(level.clamp(0.0, 1.0), std::ptr::null());
        },
        Ok(None) => {}
        Err(error) => eprintln!("session_volume: failed to look up session for pid {pid}: {error:?}"),
    }
}
