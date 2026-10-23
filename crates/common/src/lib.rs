use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const EQ_BAND_COUNT: usize = 5;

/// A saved 5-band EQ setting, keyed by bus name ("Game", "Media", "Aux",
/// "Mic", ...) in [`Presets::eq`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqPreset {
    pub gains_db: [f32; EQ_BAND_COUNT],
}

/// An EQ setting you saved under a name, to pick for any channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedEq {
    pub name: String,
    pub gains_db: [f32; EQ_BAND_COUNT],
}

impl Default for EqPreset {
    fn default() -> Self {
        Self {
            gains_db: [0.0; EQ_BAND_COUNT],
        }
    }
}

/// Everything Sonora remembers between runs (`config.json`). Each
/// section loads on its own, so one unreadable section doesn't cost the
/// others.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Presets {
    /// EQ per bus name.
    #[serde(default)]
    pub eq: HashMap<String, EqPreset>,
    #[serde(default)]
    pub moments: MomentsConfig,
    #[serde(default)]
    pub mixer: MixerSetup,
    /// EQ settings you saved by name.
    #[serde(default)]
    pub eq_presets: Vec<NamedEq>,
    #[serde(default)]
    pub mic: MicConfig,
}

/// How the microphone is cleaned up before it reaches the mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MicConfig {
    pub noise_suppression: NoiseSuppression,
    /// Also turn down voices well below yours (people further from the
    /// microphone). A level heuristic, not speaker recognition.
    pub quiet_far_voices: bool,
}

/// How hard the microphone's noise removal works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NoiseSuppression {
    Off,
    /// RNNoise: the lightest on CPU and memory; handles steady noise (fans,
    /// hum) but lets more keyboard clatter through.
    Light,
    /// DeepFilterNet 3, holding back from removing everything so voices
    /// keep some room sound.
    Standard,
    /// DeepFilterNet 3 at full strength, like Krisp.
    #[default]
    Strong,
}

/// The mixer as it was left: levels, devices, and which apps (and
/// microphone) feed which bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerSetup {
    pub master_gain: f32,
    /// Per bus name.
    pub buses: HashMap<String, SavedBus>,
    pub sources: Vec<SavedSource>,
}

impl Default for MixerSetup {
    fn default() -> Self {
        Self { master_gain: 1.0, buses: HashMap::new(), sources: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBus {
    pub gain: f32,
    pub muted: bool,
    /// Output device id as Windows reports it; `None` plays to the system
    /// default device.
    pub device: Option<String>,
    /// When `device` disconnects: play on the default device until it's
    /// back (true), or go silent (false).
    pub fallback_to_default: bool,
    /// Whether you hear the bus; `None` (settings from before this) uses
    /// the app's default for that bus.
    pub heard: Option<bool>,
    /// `None` (settings from before this) uses the app's default.
    pub priority: Option<Priority>,
    /// Whether a copy also plays on the default device (hearing your own
    /// mic); `None` is off.
    pub monitor: Option<bool>,
    /// `#RRGGBB` shown for the bus instead of its built-in color.
    pub color: Option<String>,
}

impl Default for SavedBus {
    fn default() -> Self {
        Self { gain: 1.0, muted: false, device: None, fallback_to_default: true, heard: None, priority: None, monitor: None, color: None }
    }
}

/// How a channel trades delay against safety from dropouts (crackles) when
/// your PC is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    /// Least delay: for games and voice, where late sound is noticeable.
    High,
    Normal,
    /// Most headroom against dropouts: for music and video.
    Low,
}

/// One app (or the microphone) routed into a bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSource {
    /// The app's executable name, lowercase (e.g. "cider.exe"), since its
    /// process id changes every run; `None` for the microphone.
    pub app: Option<String>,
    pub bus: String,
    pub gain: f32,
    pub muted: bool,
    /// The microphone's capture device id; `None` is the system default.
    #[serde(default)]
    pub input_device: Option<String>,
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
    /// `None` saves to the default folder (`Videos\sonora`).
    pub clips_dir: Option<PathBuf>,
    /// Clip volume per track (bus name); a missing track is at 100%.
    pub track_volumes: HashMap<String, f32>,
    pub hotkey: Hotkey,
    /// What clips record; `None` until you pick (the Clips tab asks),
    /// recording the whole screen meanwhile.
    pub record: Option<RecordMode>,
    /// Apps (lowercase executable names) recorded like games.
    pub always_record: Vec<String>,
    /// Detected games (lowercase executable names) never recorded.
    pub never_record: Vec<String>,
}

/// What clips record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordMode {
    /// The whole primary monitor, always.
    Screen,
    /// Only the window of a running game (or an app you chose), and
    /// nothing while none is open.
    Games,
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
            record: None,
            always_record: Vec::new(),
            never_record: Vec::new(),
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

/// `%APPDATA%\sonora`, where every Sonora settings file lives.
pub fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("sonora")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Where settings lived before `config.json`; moved there on first load.
fn legacy_config_path() -> PathBuf {
    config_dir().join("presets.json")
}

/// Apps whose own volume Sonora turned down to route them through the
/// mixer, with the volume each had before (keyed by lowercase executable
/// name). Kept on disk so a crash can't leave an app near-silent: Windows
/// remembers per-app volumes, so the next run restores leftovers.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedApps {
    pub original_volumes: HashMap<String, f32>,
}

fn routed_apps_path() -> PathBuf {
    config_dir().join("routed-apps.json")
}

pub fn load_routed_apps() -> RoutedApps {
    std::fs::read_to_string(routed_apps_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_routed_apps(apps: &RoutedApps) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    let json = serde_json::to_string_pretty(apps).map_err(std::io::Error::other)?;
    std::fs::write(routed_apps_path(), json)
}

/// Problems found loading settings, for the UI to show once.
static CONFIG_NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Takes the settings problems found since the last call: a settings file
/// that couldn't be read and what was done about it.
pub fn take_config_notices() -> Vec<String> {
    std::mem::take(&mut *CONFIG_NOTICES.lock().unwrap())
}

/// Loads `config.json`, or defaults if there isn't one yet. Never fails: an
/// unreadable file is set aside (never overwritten) and the last good
/// backup, or defaults, are used instead; see [`take_config_notices`].
pub fn load_presets() -> Presets {
    let path = config_path();
    if !path.exists() && legacy_config_path().exists() {
        let _ = std::fs::rename(legacy_config_path(), &path);
    }
    let (presets, notices) = load_from(&path);
    CONFIG_NOTICES.lock().unwrap().extend(notices);
    presets
}

/// Loads the settings file, applies `change`, and writes it back, so each
/// part of the app can update its own section without clobbering others.
pub fn update_presets(change: impl FnOnce(&mut Presets)) -> std::io::Result<()> {
    let mut presets = load_presets();
    change(&mut presets);
    save_presets(&presets)
}

pub fn save_presets(presets: &Presets) -> std::io::Result<()> {
    save_to(&config_path(), presets)
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn load_from(path: &Path) -> (Presets, Vec<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Presets::default(), Vec::new()),
        Err(error) => {
            // Possibly temporary (locked by another program): leave the file
            // alone. Saving keeps a backup of it first.
            return (Presets::default(), vec![format!("Couldn't read {}: {error}. Using default settings.", path.display())]);
        }
    };
    let (presets, broken) = parse_sections(&text);
    if broken.is_empty() {
        return (presets, Vec::new());
    }

    // Keep the unreadable file for the user, and out of the way so the next
    // save neither overwrites it nor backs it up over the good backup.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let kept = path.with_extension(format!("corrupt-{stamp}.json"));
    let kept_note = match std::fs::rename(path, &kept) {
        Ok(()) => format!("The unreadable file was kept as {}.", kept.display()),
        Err(error) => format!("It couldn't be set aside ({error})."),
    };

    if broken.iter().any(|section| section == WHOLE_FILE) {
        if let Ok(text) = std::fs::read_to_string(backup_path(path)) {
            let (backup, backup_broken) = parse_sections(&text);
            if backup_broken.is_empty() {
                return (backup, vec![format!("Settings were unreadable; restored the previous version. {kept_note}")]);
            }
        }
        return (presets, vec![format!("Settings were unreadable and reset to defaults. {kept_note}")]);
    }
    let note = format!("Some settings were unreadable and reset to defaults ({}). {kept_note}", broken.join(", "));
    (presets, vec![note])
}

const WHOLE_FILE: &str = "the whole file";

/// Parses each section on its own; returns the names of those that had to
/// fall back to defaults.
fn parse_sections(text: &str) -> (Presets, Vec<String>) {
    let Ok(serde_json::Value::Object(mut sections)) = serde_json::from_str(text) else {
        return (Presets::default(), vec![WHOLE_FILE.to_string()]);
    };
    let mut broken = Vec::new();
    let mut section = |name: &str| sections.remove(name).unwrap_or(serde_json::Value::Null);
    fn parse<T: Default + serde::de::DeserializeOwned>(name: &str, value: serde_json::Value, broken: &mut Vec<String>) -> T {
        if value.is_null() {
            return T::default();
        }
        serde_json::from_value(value).unwrap_or_else(|_| {
            broken.push(name.to_string());
            T::default()
        })
    }
    let (eq, moments, mixer, eq_presets, mic) =
        (section("eq"), section("moments"), section("mixer"), section("eq_presets"), section("mic"));
    let presets = Presets {
        eq: parse("EQ", eq, &mut broken),
        moments: parse("clip settings", moments, &mut broken),
        mixer: parse("mixer", mixer, &mut broken),
        eq_presets: parse("EQ presets", eq_presets, &mut broken),
        mic: parse("microphone noise removal", mic, &mut broken),
    };
    (presets, broken)
}

/// Writes to a temporary file and moves it into place, so a crash mid-write
/// can't leave a half-written settings file; the previous file is kept as
/// `config.json.bak`.
fn save_to(path: &Path, presets: &Presets) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(presets).map_err(std::io::Error::other)?;
    let partial = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        let _ = std::fs::copy(path, backup_path(path));
    }
    std::fs::rename(&partial, path)
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
        assert_eq!(presets.mixer, MixerSetup::default());
    }

    #[test]
    fn buses_saved_before_the_disconnect_choice_keep_playing_on_default() {
        let old = r#"{ "gain": 0.5, "muted": false, "device": "{0.0.0.00000000}.{tv}" }"#;
        let bus: SavedBus = serde_json::from_str(old).unwrap();
        assert!(bus.fallback_to_default);
        assert_eq!(bus.device.as_deref(), Some("{0.0.0.00000000}.{tv}"));
    }

    #[test]
    fn mixer_setup_round_trips() {
        let mut presets = Presets::default();
        presets.mixer.sources.push(SavedSource {
            app: Some("cider.exe".into()),
            bus: "Aux".into(),
            gain: 0.3,
            muted: false,
            input_device: None,
        });
        presets.mixer.sources.push(SavedSource {
            app: None,
            bus: "Mic".into(),
            gain: 1.0,
            muted: true,
            input_device: Some("{0.0.1.00000000}.{mic}".into()),
        });
        presets.mixer.buses.insert(
            "Aux".into(),
            SavedBus { gain: 0.5, muted: false, device: Some("{0.0.0.00000000}.{tv}".into()), fallback_to_default: false, heard: Some(false), priority: Some(Priority::Low), monitor: Some(true), color: Some("#A855F7".into()) },
        );
        let json = serde_json::to_string(&presets).unwrap();
        let loaded: Presets = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mixer, presets.mixer);
    }

    /// A fresh, empty folder for one test's settings files.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sonora-config-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_loads_defaults_quietly() {
        let dir = test_dir("missing");
        let (presets, notices) = load_from(&dir.join("config.json"));
        assert_eq!(presets.mixer, MixerSetup::default());
        assert!(notices.is_empty());
    }

    #[test]
    fn save_keeps_the_previous_file_as_a_backup() {
        let dir = test_dir("backup");
        let path = dir.join("config.json");
        let mut presets = Presets::default();
        presets.mixer.master_gain = 0.5;
        save_to(&path, &presets).unwrap();
        presets.mixer.master_gain = 0.25;
        save_to(&path, &presets).unwrap();
        assert_eq!(files_in(&dir), ["config.json", "config.json.bak"]);
        assert_eq!(load_from(&path).0.mixer.master_gain, 0.25);
        assert_eq!(load_from(&backup_path(&path)).0.mixer.master_gain, 0.5);
    }

    #[test]
    fn unreadable_file_is_set_aside_and_the_backup_restored() {
        let dir = test_dir("corrupt");
        let path = dir.join("config.json");
        let mut presets = Presets::default();
        presets.mixer.master_gain = 0.5;
        save_to(&path, &presets).unwrap();
        save_to(&path, &presets).unwrap();
        std::fs::write(&path, "{ \"mixer\": { truncated").unwrap();

        let (loaded, notices) = load_from(&path);
        assert_eq!(loaded.mixer.master_gain, 0.5, "restored from config.json.bak");
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("restored the previous version"), "{notices:?}");
        let files = files_in(&dir);
        assert!(!files.contains(&"config.json".to_string()), "the broken file is moved away: {files:?}");
        let kept = files.iter().find(|f| f.contains("corrupt")).expect("broken file kept");
        assert_eq!(std::fs::read_to_string(dir.join(kept)).unwrap(), "{ \"mixer\": { truncated");

        // The next save must not back up the broken file over the good backup.
        save_to(&path, &loaded).unwrap();
        assert_eq!(load_from(&backup_path(&path)).0.mixer.master_gain, 0.5);
    }

    #[test]
    fn one_bad_section_keeps_the_others() {
        let dir = test_dir("section");
        let path = dir.join("config.json");
        let text = r#"{ "eq": { "Game": { "gains_db": [1.0, 0.0, 0.0, 0.0, 0.0] } },
                        "moments": { "buffer_secs": 60 },
                        "mixer": { "master_gain": "loud" } }"#;
        std::fs::write(&path, text).unwrap();
        let (presets, notices) = load_from(&path);
        assert_eq!(presets.eq["Game"].gains_db[0], 1.0);
        assert_eq!(presets.moments.buffer_secs, 60);
        assert_eq!(presets.mixer, MixerSetup::default());
        assert!(notices[0].contains("(mixer)"), "{notices:?}");
        assert!(files_in(&dir).iter().any(|f| f.contains("corrupt")));
    }

    #[test]
    fn unreadable_file_without_backup_resets_to_defaults() {
        let dir = test_dir("nobackup");
        let path = dir.join("config.json");
        std::fs::write(&path, "not json").unwrap();
        let (presets, notices) = load_from(&path);
        assert_eq!(presets.mixer, MixerSetup::default());
        assert!(notices[0].contains("reset to defaults"), "{notices:?}");
    }

    #[test]
    fn settings_from_before_noise_removal_get_strong() {
        let (presets, broken) = parse_sections(r#"{ "eq": {} }"#);
        assert!(broken.is_empty());
        assert_eq!(presets.mic.noise_suppression, NoiseSuppression::Strong);
        assert!(!presets.mic.quiet_far_voices);
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
