//! What the mixer sets up on the sound server while it runs: the virtual
//! microphone. Starting also removes what a crashed run left behind.

use super::conn::module_text;
use super::{Connection, APP_SINK_PREFIX, VIRTUAL_MIC, VIRTUAL_MIC_NAME, VOICE_SINK};
use crate::error::Result;

pub(crate) struct MixerSetup {
    /// The virtual microphone's modules, unloaded (last first) when the
    /// mixer stops. Empty if it couldn't be set up.
    modules: Vec<u32>,
}

impl MixerSetup {
    pub(crate) fn start() -> Result<Self> {
        let mut connection = Connection::open("Sonora setup")?;
        let modules = connection.modules()?;
        // A routed app's sink outliving its run keeps the app silent;
        // removing it hands the app back to the default output. Only one
        // Sonora runs at a time, so any such sink is a leftover.
        for module in &modules {
            if module.argument.contains(&format!("sink_name={APP_SINK_PREFIX}")) {
                let _ = connection.unload_module(module.index);
            }
        }
        // Keep a virtual microphone a crashed run left (Discord stays set to
        // it), or set one up.
        let existing = |name: &str| modules.iter().find(|m| m.argument.contains(name)).map(|m| m.index);
        let voice_sink = match existing(&format!("sink_name={VOICE_SINK} ")) {
            Some(index) => Some(index),
            None => connection
                .load_module(
                    "module-null-sink",
                    &format!(
                        "sink_name={VOICE_SINK} sink_properties=\"device.description='{}'\"",
                        module_text("Sonora voice (for the virtual microphone)")
                    ),
                )
                .ok(),
        };
        let virtual_mic = match (voice_sink, existing(&format!("source_name={VIRTUAL_MIC} "))) {
            (_, Some(index)) => Some(index),
            (Some(_), None) => connection
                .load_module(
                    "module-remap-source",
                    &format!(
                        "master={VOICE_SINK}.monitor source_name={VIRTUAL_MIC} source_properties=\"device.description='{}'\"",
                        module_text(VIRTUAL_MIC_NAME)
                    ),
                )
                .ok(),
            (None, None) => None,
        };
        Ok(Self { modules: voice_sink.into_iter().chain(virtual_mic).collect() })
    }
}

impl Drop for MixerSetup {
    fn drop(&mut self) {
        if self.modules.is_empty() {
            return;
        }
        if let Ok(mut connection) = Connection::open("Sonora setup") {
            for &module in self.modules.iter().rev() {
                let _ = connection.unload_module(module);
            }
        }
    }
}
