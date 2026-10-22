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
    #[serde(default)]
    pub moments: MomentsConfig,
}

/// Moments (clip recording) settings. Every field has a default, so older
/// settings files load unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MomentsConfig {
    pub enabled: bool,
    /// How far back a saved clip reaches.
    pub buffer_secs: u32,
    pub fps: u32,
    /// Encoded height: 720, 1080, 1440 or 2160.
    pub height: u32,
    /// `None` saves to the default folder (`Videos\sonar-rs`).
    pub clips_dir: Option<PathBuf>,
    /// Clip volume per track (bus name); a missing track is at 100%.
    pub track_volumes: HashMap<String, f32>,
    pub hotkey: Hotkey,
}

impl Default for MomentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            buffer_secs: 30,
            fps: 60,
            height: 1080,
            clips_dir: None,
            track_volumes: HashMap::new(),
            hotkey: Hotkey::default(),
        }
    }
}

/// A global keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Windows virtual-key code of the non-modifier key.
    pub key: u32,
}

impl Default for Hotkey {
    /// Alt+F10, the "save the last N seconds" key most people already know.
    fn default() -> Self {
        Self { ctrl: false, alt: true, shift: false, key: 0x79 }
    }
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

/// Loads the settings file, applies `change`, and writes it back, so each
/// part of the app can update its own section without clobbering others.
pub fn update_presets(change: impl FnOnce(&mut Presets)) -> std::io::Result<()> {
    let mut presets = load_presets();
    change(&mut presets);
    save_presets(&presets)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_before_moments_load_with_moments_defaults() {
        let old = r#"{ "eq": { "Game": { "gains_db": [1.0, 0.0, 0.0, 0.0, 0.0] } } }"#;
        let presets: Presets = serde_json::from_str(old).unwrap();
        assert_eq!(presets.eq["Game"].gains_db[0], 1.0);
        assert_eq!(presets.moments, MomentsConfig::default());
    }

    #[test]
    fn partial_moments_section_fills_in_missing_fields() {
        let partial = r#"{ "eq": {}, "moments": { "buffer_secs": 60 } }"#;
        let presets: Presets = serde_json::from_str(partial).unwrap();
        assert_eq!(presets.moments.buffer_secs, 60);
        assert_eq!(presets.moments.fps, 60);
        assert_eq!(presets.moments.hotkey, Hotkey::default());
    }
}
