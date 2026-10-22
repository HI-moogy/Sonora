use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const EQ_BAND_COUNT: usize = 5;

/// A saved 5-band EQ setting, keyed by bus name ("Game", "Media", "Aux",
/// "Mic", ...) in [`Presets::eq`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EqPreset {
    pub gains_db: [f32; EQ_BAND_COUNT],
}

impl Default for EqPreset {
    fn default() -> Self {
        Self {
            gains_db: [0.0; EQ_BAND_COUNT],
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Presets {
    pub eq: HashMap<String, EqPreset>,
}

fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(base);
    path.push("sonar-rs");
    path.push("presets.json");
    path
}

/// Loads saved presets from disk, or an empty set if none have been saved
/// yet (never an error: a missing or unreadable file just means defaults).
pub fn load_presets() -> Presets {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_presets(presets: &Presets) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(presets)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}
