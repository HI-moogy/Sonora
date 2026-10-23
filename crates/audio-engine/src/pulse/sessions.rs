//! Apps playing audio: the sound server's playback streams.

use super::conn::with_control;
use super::procs;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct AudioSession {
    pub process_id: u32,
    pub display_name: String,
    /// Event sounds (notifications, clicks) rather than an app.
    pub is_system_sounds: bool,
}

/// Every app with a playback stream, once per stream. A helper process
/// playing for an app is listed as that app (see [`procs::app_root`]).
/// Sonora's own streams aren't listed.
pub fn enumerate_sessions() -> Result<Vec<AudioSession>> {
    let own_pid = std::process::id();
    Ok(with_control(|connection| connection.sink_inputs())?
        .into_iter()
        .filter_map(|input| {
            // Sonora's own streams (or a helper of its own) aren't an app.
            let pid = input.pid.map(procs::app_root).filter(|&pid| pid != own_pid)?;
            // The program's name reads better than what some apps call
            // their stream (Discord's voice is "WEBRTC VoiceEngine").
            let display_name = input
                .binary
                .or_else(|| procs::executable_name(pid))
                .or(input.app_name)
                .unwrap_or_default();
            Some(AudioSession { process_id: pid, display_name, is_system_sounds: input.is_event })
        })
        .collect())
}

/// The program's file name ("firefox", "Discord").
pub fn process_executable_name(pid: u32) -> Option<String> {
    procs::executable_name(pid)
}
