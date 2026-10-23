//! Outputs and microphones, as the sound server lists them.

use super::conn::with_control;
use super::{PREFIX, VIRTUAL_MIC_NAME, VOICE_SINK};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// The server's name for it, which stays the same between runs.
    pub id: String,
    pub name: String,
    /// For an output that's a virtual cable (Sonora's own voice output):
    /// the microphone its other end shows up as in Discord and games.
    pub voice_cable_mic: Option<String>,
}

/// Every output, plus Sonora's voice output for the Mic channel. Sonora's
/// other sinks (routed apps') aren't outputs you'd pick.
pub fn list_output_devices() -> Result<Vec<AudioDevice>> {
    Ok(with_control(|connection| connection.sinks())?
        .into_iter()
        .filter(|sink| !sink.name.starts_with(PREFIX) || sink.name == VOICE_SINK)
        .map(|sink| {
            let voice = sink.name == VOICE_SINK;
            AudioDevice {
                name: if voice { VIRTUAL_MIC_NAME.to_string() } else { sink.description },
                id: sink.name,
                voice_cable_mic: voice.then(|| VIRTUAL_MIC_NAME.to_string()),
            }
        })
        .collect())
}

/// Every microphone and line-in: not outputs' monitors, and not Sonora's
/// own virtual microphone (recording it would feed your voice back in).
pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    Ok(with_control(|connection| connection.sources())?
        .into_iter()
        .filter(|source| !source.is_monitor && !source.name.starts_with(PREFIX))
        .map(|source| AudioDevice { id: source.name, name: source.description, voice_cable_mic: None })
        .collect())
}
