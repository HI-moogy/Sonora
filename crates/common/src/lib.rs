use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// How many bands a channel's EQ has.
pub const EQ_BANDS: usize = 10;

/// What one EQ band does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqKind {
    /// Raises or lowers the frequencies around its own.
    #[default]
    Peak,
    /// Raises or lowers everything below it.
    LowShelf,
    /// Raises or lowers everything above it.
    HighShelf,
    /// Takes away everything below it.
    LowCut,
    /// Takes away everything above it.
    HighCut,
}

/// One EQ band: where it is (Hz), how much (dB, for peaks and shelves)
/// and how wide (Q: higher is narrower).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: EqKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    pub on: bool,
}

/// The Q of a peak an octave wide.
pub const OCTAVE_Q: f32 = 1.41;

impl EqBand {
    pub const fn peak(freq: f32) -> Self {
        Self { kind: EqKind::Peak, freq, gain_db: 0.0, q: OCTAVE_Q, on: true }
    }

    /// Whether it changes the sound: cuts always do, peaks and shelves when
    /// they raise or lower.
    pub fn shapes(&self) -> bool {
        self.on
            && match self.kind {
                EqKind::Peak | EqKind::LowShelf | EqKind::HighShelf => self.gain_db.abs() >= 0.01,
                EqKind::LowCut | EqKind::HighCut => true,
            }
    }
}

/// Where a channel's bands start: an octave apart, flat, like a graphic EQ.
pub const DEFAULT_EQ_BANDS: [EqBand; EQ_BANDS] = [
    EqBand::peak(31.25),
    EqBand::peak(62.5),
    EqBand::peak(125.0),
    EqBand::peak(250.0),
    EqBand::peak(500.0),
    EqBand::peak(1000.0),
    EqBand::peak(2000.0),
    EqBand::peak(4000.0),
    EqBand::peak(8000.0),
    EqBand::peak(16_000.0),
];

/// Older Sonora's five bands (100 Hz to 10 kHz, Q 1).
const FIVE_BAND_FREQUENCIES: [f32; 5] = [100.0, 350.0, 1000.0, 3500.0, 10_000.0];

/// The ten bands that sound exactly as older Sonora's five did with these
/// gains: each of the five takes the place of the band nearest it.
pub fn bands_from_five(gains_db: [f32; 5]) -> [EqBand; EQ_BANDS] {
    let mut bands = DEFAULT_EQ_BANDS;
    let mut placed = [false; EQ_BANDS];
    for (freq, gain_db) in FIVE_BAND_FREQUENCIES.into_iter().zip(gains_db) {
        let distance = |band: &EqBand| (band.freq / freq).ln().abs();
        let nearest = (0..EQ_BANDS)
            .filter(|&index| !placed[index])
            .min_by(|&a, &b| distance(&bands[a]).total_cmp(&distance(&bands[b])))
            .expect("more bands than older Sonora had");
        bands[nearest] = EqBand { kind: EqKind::Peak, freq, gain_db, q: 1.0, on: true };
        placed[nearest] = true;
    }
    bands
}

/// A channel's EQ as saved, keyed by bus name ("Game", "Media", "Aux",
/// "Mic", ...) in [`Presets::eq`]. Settings from older Sonora, with five
/// gains, load as the ten bands that sound the same.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedEq")]
pub struct EqPreset {
    /// Off, the EQ is bypassed but keeps its bands.
    pub enabled: bool,
    pub bands: [EqBand; EQ_BANDS],
}

impl Default for EqPreset {
    fn default() -> Self {
        Self { enabled: true, bands: DEFAULT_EQ_BANDS }
    }
}

/// An EQ setting you saved under a name, to pick for any channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedNamedEq")]
pub struct NamedEq {
    pub name: String,
    pub bands: [EqBand; EQ_BANDS],
}

/// A saved EQ, as this version or older ones wrote it.
#[derive(Deserialize)]
struct SavedEq {
    enabled: Option<bool>,
    bands: Option<Vec<EqBand>>,
    gains_db: Option<[f32; 5]>,
}

#[derive(Deserialize)]
struct SavedNamedEq {
    name: String,
    #[serde(flatten)]
    eq: SavedEq,
}

impl SavedEq {
    fn bands(self) -> [EqBand; EQ_BANDS] {
        match (self.bands, self.gains_db) {
            (Some(saved), _) => std::array::from_fn(|index| saved.get(index).copied().unwrap_or(DEFAULT_EQ_BANDS[index])),
            (None, Some(gains_db)) => bands_from_five(gains_db),
            (None, None) => DEFAULT_EQ_BANDS,
        }
    }
}

impl From<SavedEq> for EqPreset {
    fn from(saved: SavedEq) -> Self {
        Self { enabled: saved.enabled.unwrap_or(true), bands: saved.bands() }
    }
}

impl From<SavedNamedEq> for NamedEq {
    fn from(saved: SavedNamedEq) -> Self {
        Self { name: saved.name, bands: saved.eq.bands() }
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
    /// Your colors; `None` until first saved (older settings then bring
    /// their channel colors along, see [`SavedBus::color`]).
    #[serde(default)]
    pub colors: Option<ThemeColors>,
    #[serde(default)]
    pub guide: GuideState,
}

/// Whether you've been through the getting-started guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuideState {
    pub seen: bool,
}

/// A color, saved as `#RRGGBB` so it's easy to read, edit and share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b])
    }

    /// Reads `#RRGGBB` or `RRGGBB`.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim();
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Self([channel(0)?, channel(2)?, channel(4)?]))
    }

    pub fn to_hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| serde::de::Error::custom(format!("{text:?} isn't a #RRGGBB color")))
    }
}

/// Every color Sonora draws with. Colors missing from a saved theme (one
/// from before a color was added) are Sonora's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    /// Behind everything.
    pub background: Rgb,
    /// Side panels and windows.
    pub panels: Rgb,
    /// Cards, the boxes things are grouped in.
    pub cards: Rgb,
    /// Buttons, fields and slider rails.
    pub controls: Rgb,
    pub controls_hovered: Rgb,
    pub borders: Rgb,
    pub text: Rgb,
    /// Hints, labels and details.
    pub text_muted: Rgb,
    /// Selected tabs, main buttons, sliders' filled part.
    pub accent: Rgb,
    /// Text and icons on the accent color.
    pub text_on_accent: Rgb,
    pub warning: Rgb,
    /// Errors and muted channels.
    pub error: Rgb,
    /// Game, Chat, Aux, Media, Mic.
    pub channels: [Rgb; 5],
    /// The clip editor's desktop audio track.
    pub desktop_track: Rgb,
}

impl ThemeColors {
    /// Sonora's own look: dark, with a warm coral accent.
    pub const SONORA: Self = Self {
        background: Rgb::new(0x12, 0x14, 0x19),
        panels: Rgb::new(0x19, 0x1C, 0x23),
        cards: Rgb::new(0x15, 0x18, 0x1F),
        controls: Rgb::new(0x22, 0x26, 0x2F),
        controls_hovered: Rgb::new(0x2B, 0x30, 0x3B),
        borders: Rgb::new(0x2C, 0x31, 0x3C),
        text: Rgb::new(0xE7, 0xE9, 0xEE),
        text_muted: Rgb::new(0x8C, 0x93, 0xA3),
        accent: Rgb::new(0xFF, 0x7A, 0x59),
        text_on_accent: Rgb::new(0x1A, 0x10, 0x0C),
        warning: Rgb::new(0xF0, 0xB4, 0x4C),
        error: Rgb::new(0xF2, 0x6D, 0x6D),
        channels: [
            Rgb::new(0x4F, 0xA3, 0xFF),
            Rgb::new(0x45, 0xD0, 0x8A),
            Rgb::new(0xA7, 0x8B, 0xFA),
            Rgb::new(0xF4, 0x72, 0xB6),
            Rgb::new(0xFB, 0xBF, 0x24),
        ],
        desktop_track: Rgb::new(0x96, 0x96, 0xA0),
    };
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::SONORA
    }
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
    /// The output nobody listens to that apps the mixer plays put their own
    /// sound on, so streams of them have it; `None` turns them down.
    pub silent_output: Option<String>,
}

impl Default for MixerSetup {
    fn default() -> Self {
        Self { master_gain: 1.0, buses: HashMap::new(), sources: Vec::new(), silent_output: None }
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
    /// Virtual surround; off in settings from before it.
    pub surround: Surround,
    /// Older settings' `#RRGGBB` color for the bus. Read only: it moves
    /// into [`ThemeColors::channels`] the first time colors are saved.
    #[serde(skip_serializing)]
    pub color: Option<String>,
}

impl Default for SavedBus {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
            device: None,
            fallback_to_default: true,
            heard: None,
            priority: None,
            monitor: None,
            surround: Surround::default(),
            color: None,
        }
    }
}

/// Virtual surround for headphones: a channel played as if from speakers in
/// a room around you, rather than inside your head.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Surround {
    pub enabled: bool,
    /// How far away the speakers are, in meters, from [`Self::MIN_DISTANCE_M`]
    /// to [`Self::MAX_DISTANCE_M`]: farther brings more of the room.
    pub distance_m: f32,
}

impl Surround {
    pub const MIN_DISTANCE_M: f32 = 0.5;
    pub const MAX_DISTANCE_M: f32 = 4.0;
    pub const DEFAULT_DISTANCE_M: f32 = 1.5;
}

impl Default for Surround {
    fn default() -> Self {
        Self { enabled: false, distance_m: Self::DEFAULT_DISTANCE_M }
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
    /// recording like [`RecordMode::ScreenWhilePlaying`] meanwhile.
    pub record: Option<RecordMode>,
    /// Apps recorded like games. Each is lowercase: a program's file name
    /// ("cider.exe"), or a path with a backslash (a folder ending in `\`
    /// covers every program in it).
    pub always_record: Vec<String>,
    /// Detected games never recorded, written like `always_record`.
    pub never_record: Vec<String>,
    /// How the Clips tab orders clips.
    pub clip_sort: ClipSort,
    /// The size each copy for Discord stays under, in MB (Discord's upload
    /// limit for your account).
    pub share_limit_mb: u32,
}

/// How the Clips tab orders clips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipSort {
    #[default]
    NewestFirst,
    OldestFirst,
    /// Grouped by game (A to Z), newest first in each.
    ByGame,
}

/// What clips record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordMode {
    /// The whole primary monitor, always.
    Screen,
    /// The whole monitor a running game (or an app you chose) is on, and
    /// nothing while none is open.
    ScreenWhilePlaying,
    /// Only the window of a running game (or an app you chose), and
    /// nothing while none is open.
    Games,
}

impl RecordMode {
    /// What clips record until you pick: only while a game is open, since
    /// recording the screen nonstop keeps the GPU's video encoder busy.
    pub const UNTIL_PICKED: Self = Self::ScreenWhilePlaying;
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
            clip_sort: ClipSort::NewestFirst,
            share_limit_mb: 20,
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

/// Where every Sonora settings file lives: `%APPDATA%\sonora` on Windows,
/// `~/.config/sonora` (or `$XDG_CONFIG_HOME/sonora`) elsewhere.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.unwrap_or_else(|| PathBuf::from(".")).join("sonora")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Where settings lived before `config.json`; moved there on first load.
fn legacy_config_path() -> PathBuf {
    config_dir().join("presets.json")
}

/// Apps whose own volume Sonora changed to route them, with the volume each
/// had before (keyed by lowercase executable name). Kept on disk so a crash
/// can't leave an app near-silent: Windows remembers per-app volumes, so
/// the next run restores leftovers.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutedApps {
    pub original_volumes: HashMap<String, f32>,
    /// Apps playing straight on their channel's device: Sonora set their
    /// volume to the channel's rather than turning them down, so they're
    /// restored like the others but never left silent.
    pub direct: HashSet<String>,
    /// Apps Sonora pointed at their channel's device with Windows' per-app
    /// output setting, with the device each was set to before (`None`: the
    /// default device).
    pub pointed: HashMap<String, Option<String>>,
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

/// The tags you made for clips, and which clips have them
/// (`clip-tags.json`, apart from the settings: it's about your clips).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipTags {
    /// Your tags, in the order you made them.
    pub tags: Vec<ClipTag>,
    /// Each tagged clip's tags (by name), by the clip's full path.
    pub clips: BTreeMap<PathBuf, Vec<String>>,
    /// The game you said a clip is from, where its file and folder said
    /// nothing or something wrong (Radeon's "unknown" replays), by path.
    pub games: BTreeMap<PathBuf, String>,
}

/// One of your clip tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipTag {
    pub name: String,
    /// What it looks like on a clip.
    pub color: Rgb,
}

/// Why a tag name wasn't taken.
#[derive(Debug, PartialEq)]
pub enum TagError {
    Empty,
    TooLong,
    /// Another tag has that name already (it's given back, as spelled).
    Exists(String),
}

impl ClipTags {
    /// The longest a tag can be, so it fits on a clip's card.
    pub const MAX_LEN: usize = 24;

    /// `clip`'s tags, in your tags' order.
    pub fn of(&self, clip: &Path) -> Vec<&ClipTag> {
        let Some(on) = self.clips.get(clip) else { return Vec::new() };
        self.tags.iter().filter(|tag| on.contains(&tag.name)).collect()
    }

    pub fn has(&self, clip: &Path, tag: &str) -> bool {
        self.clips.get(clip).is_some_and(|on| on.iter().any(|t| t == tag))
    }

    /// Puts `tag` on `clip`, or takes it off.
    pub fn set(&mut self, clip: &Path, tag: &str, on: bool) {
        let tags = self.clips.entry(clip.to_path_buf()).or_default();
        tags.retain(|t| t != tag);
        if on {
            tags.push(tag.to_string());
        }
        if tags.is_empty() {
            self.clips.remove(clip);
        }
    }

    /// Makes a tag (`name` trimmed); returns its name as saved.
    pub fn add_tag(&mut self, name: &str, color: Rgb) -> Result<String, TagError> {
        let name = self.free_name(name, None)?;
        self.tags.push(ClipTag { name: name.clone(), color });
        Ok(name)
    }

    /// Renames a tag, on every clip that has it; returns the new name as
    /// saved.
    pub fn rename_tag(&mut self, tag: &str, name: &str) -> Result<String, TagError> {
        let name = self.free_name(name, Some(tag))?;
        for saved in self.tags.iter_mut().filter(|saved| saved.name == tag) {
            saved.name = name.clone();
        }
        for on in self.clips.values_mut() {
            for saved in on.iter_mut().filter(|saved| *saved == tag) {
                *saved = name.clone();
            }
        }
        Ok(name)
    }

    pub fn set_color(&mut self, tag: &str, color: Rgb) {
        for saved in self.tags.iter_mut().filter(|saved| saved.name == tag) {
            saved.color = color;
        }
    }

    /// The game you said `clip` is from, if you did.
    pub fn game_of(&self, clip: &Path) -> Option<&str> {
        self.games.get(clip).map(String::as_str)
    }

    /// Says which game `clip` is from (trimmed); `None` or nothing typed
    /// goes back to what its file and folder say.
    pub fn set_game(&mut self, clip: &Path, game: Option<&str>) {
        match game.map(str::trim).filter(|game| !game.is_empty()) {
            Some(game) => self.games.insert(clip.to_path_buf(), game.to_string()),
            None => self.games.remove(clip),
        };
    }

    /// Deletes a tag, taking it off every clip.
    pub fn remove_tag(&mut self, tag: &str) {
        self.tags.retain(|t| t.name != tag);
        self.clips.retain(|_, tags| {
            tags.retain(|t| t != tag);
            !tags.is_empty()
        });
    }

    /// `name` trimmed, if it can be a tag's: no other tag (but `renaming`)
    /// has it, whatever the case.
    fn free_name(&self, name: &str, renaming: Option<&str>) -> Result<String, TagError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TagError::Empty);
        }
        if name.chars().count() > Self::MAX_LEN {
            return Err(TagError::TooLong);
        }
        let taken = self.tags.iter().find(|tag| Some(tag.name.as_str()) != renaming && tag.name.to_lowercase() == name.to_lowercase());
        match taken {
            Some(tag) => Err(TagError::Exists(tag.name.clone())),
            None => Ok(name.to_string()),
        }
    }
}

fn clip_tags_path() -> PathBuf {
    config_dir().join("clip-tags.json")
}

/// Loads your clip tags. An unreadable file is set aside (kept, never
/// overwritten) and the tags start over.
pub fn load_clip_tags() -> ClipTags {
    clip_tags_from(&clip_tags_path())
}

fn clip_tags_from(path: &Path) -> ClipTags {
    // Not there yet, or can't be read right now (locked, say): left alone.
    let Ok(text) = std::fs::read_to_string(path) else { return ClipTags::default() };
    serde_json::from_str(&text).unwrap_or_else(|_| {
        let _ = std::fs::rename(path, set_aside_path(path));
        ClipTags::default()
    })
}

pub fn save_clip_tags(tags: &ClipTags) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(tags).map_err(std::io::Error::other)?;
    write_atomically(&clip_tags_path(), json.as_bytes())
}

/// Where an unreadable `file.json` is kept: `file.corrupt-<time>.json`.
fn set_aside_path(path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    path.with_extension(format!("corrupt-{stamp}.json"))
}

/// Writes to a temporary file and moves it into place, so a crash mid-write
/// can't leave a half-written file.
fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, path)
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
    let kept = set_aside_path(path);
    let set_aside = std::fs::rename(path, &kept);
    let kept_note = match &set_aside {
        Ok(()) => format!("The unreadable file was kept as {}.", kept.display()),
        Err(error) => format!("It couldn't be set aside ({error})."),
    };

    let (recovered, note) = if broken.iter().any(|section| section == WHOLE_FILE) {
        let backup = std::fs::read_to_string(backup_path(path)).ok().map(|text| parse_sections(&text));
        match backup {
            Some((backup, backup_broken)) if backup_broken.is_empty() => {
                (backup, format!("Settings were unreadable; restored the previous version. {kept_note}"))
            }
            _ => (presets, format!("Settings were unreadable and reset to defaults. {kept_note}")),
        }
    } else {
        let note = format!("Some settings were unreadable and reset to defaults ({}). {kept_note}", broken.join(", "));
        (presets, note)
    };
    // Each part of the app loads the file on its own: write back what was
    // recovered, or every later load would find no file and start from
    // defaults (and the next save would keep them). Only once the
    // unreadable file is safely set aside, never over it.
    if set_aside.is_ok() {
        let _ = save_to(path, &recovered);
    }
    (recovered, vec![note])
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
    let (eq, moments, mixer, eq_presets, mic, colors, guide) = (
        section("eq"),
        section("moments"),
        section("mixer"),
        section("eq_presets"),
        section("mic"),
        section("colors"),
        section("guide"),
    );
    let presets = Presets {
        eq: parse("EQ", eq, &mut broken),
        moments: parse("clip settings", moments, &mut broken),
        mixer: parse("mixer", mixer, &mut broken),
        eq_presets: parse("EQ presets", eq_presets, &mut broken),
        mic: parse("microphone noise removal", mic, &mut broken),
        colors: parse("colors", colors, &mut broken),
        guide: parse("guide", guide, &mut broken),
    };
    (presets, broken)
}

/// Writes to a temporary file and moves it into place, so a crash mid-write
/// can't leave a half-written settings file; the previous file is kept as
/// `config.json.bak`.
fn save_to(path: &Path, presets: &Presets) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(presets).map_err(std::io::Error::other)?;
    if path.exists() {
        let _ = std::fs::copy(path, backup_path(path));
    }
    write_atomically(path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_before_moments_load_with_moments_defaults() {
        let old = r#"{ "eq": { "Game": { "gains_db": [1.0, 0.0, 0.0, 0.0, 0.0] } } }"#;
        let presets: Presets = serde_json::from_str(old).unwrap();
        assert_eq!(presets.eq["Game"].bands.iter().find(|band| band.freq == 100.0).map(|band| band.gain_db), Some(1.0));
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
    fn buses_saved_before_surround_have_it_off() {
        let bus: SavedBus = serde_json::from_str(r#"{ "gain": 0.5, "muted": false }"#).unwrap();
        assert_eq!(bus.surround, Surround::default());
        assert!(!bus.surround.enabled);
        let partial: SavedBus = serde_json::from_str(r#"{ "surround": { "enabled": true } }"#).unwrap();
        assert_eq!(partial.surround, Surround { enabled: true, distance_m: Surround::DEFAULT_DISTANCE_M });
    }

    #[test]
    fn colors_are_saved_as_hex_and_missing_ones_are_sonoras() {
        assert_eq!(Rgb::from_hex("#FF6000"), Some(Rgb::new(0xFF, 0x60, 0x00)));
        assert_eq!(Rgb::from_hex("5865f2"), Some(Rgb::new(0x58, 0x65, 0xF2)));
        assert_eq!(Rgb::from_hex("#FF60"), None);
        assert_eq!(Rgb::from_hex("#GG6000"), None);
        assert_eq!(Rgb::from_hex("#ÿÿÿ"), None);
        let mut colors = ThemeColors::SONORA;
        colors.accent = Rgb::new(1, 2, 3);
        let json = serde_json::to_string(&colors).unwrap();
        assert!(json.contains(r##""accent":"#010203""##));
        assert_eq!(serde_json::from_str::<ThemeColors>(&json).unwrap(), colors);
        let partial: ThemeColors = serde_json::from_str(r##"{ "accent": "#010203" }"##).unwrap();
        assert_eq!(partial.background, ThemeColors::SONORA.background);
        assert_eq!(partial.accent, Rgb::new(1, 2, 3));
    }

    #[test]
    fn old_bus_colors_are_read_but_no_longer_written() {
        let bus: SavedBus = serde_json::from_str(r##"{ "color": "#A855F7" }"##).unwrap();
        assert_eq!(bus.color.as_deref(), Some("#A855F7"));
        assert!(!serde_json::to_string(&bus).unwrap().contains("color"));
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
            SavedBus {
                gain: 0.5,
                muted: false,
                device: Some("{0.0.0.00000000}.{tv}".into()),
                fallback_to_default: false,
                heard: Some(false),
                priority: Some(Priority::Low),
                monitor: Some(true),
                surround: Surround { enabled: true, distance_m: 3.0 },
                color: None,
            },
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
        let kept = files.iter().find(|f| f.contains("corrupt")).expect("broken file kept");
        assert_eq!(std::fs::read_to_string(dir.join(kept)).unwrap(), "{ \"mixer\": { truncated");
        // Loading again (each part of the app does) gets the restored
        // settings, quietly, not defaults.
        let (again, notices) = load_from(&path);
        assert_eq!(again.mixer.master_gain, 0.5);
        assert!(notices.is_empty(), "{notices:?}");

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
        assert_eq!(presets.eq["Game"].bands.iter().find(|band| band.freq == 100.0).map(|band| band.gain_db), Some(1.0));
        assert_eq!(presets.moments.buffer_secs, 60);
        assert_eq!(presets.mixer, MixerSetup::default());
        assert!(notices[0].contains("(mixer)"), "{notices:?}");
        assert!(files_in(&dir).iter().any(|f| f.contains("corrupt")));
        // The good sections survive every later load, not just the first.
        let (again, notices) = load_from(&path);
        assert_eq!(again.eq["Game"].bands.iter().find(|band| band.freq == 100.0).map(|band| band.gain_db), Some(1.0));
        assert_eq!(again.moments.buffer_secs, 60);
        assert!(notices.is_empty(), "{notices:?}");
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

    const RED: Rgb = Rgb::new(0xE5, 0x48, 0x4D);
    const BLUE: Rgb = Rgb::new(0x9A, 0xD8, 0xF0);

    fn funny_and_cool() -> ClipTags {
        let mut tags = ClipTags::default();
        tags.add_tag("Funny", RED).unwrap();
        tags.add_tag("Cool", BLUE).unwrap();
        tags
    }

    fn names<'a>(tags: impl IntoIterator<Item = &'a ClipTag>) -> Vec<&'a str> {
        tags.into_iter().map(|tag| tag.name.as_str()).collect()
    }

    #[test]
    fn eqs_from_older_sonora_keep_their_sound() {
        let preset: EqPreset = serde_json::from_str(r#"{ "gains_db": [3.0, 0.0, -2.0, 4.0, 1.0] }"#).unwrap();
        assert!(preset.enabled);
        let shaping: Vec<(f32, f32, f32)> =
            preset.bands.iter().filter(|band| band.shapes()).map(|band| (band.freq, band.gain_db, band.q)).collect();
        assert_eq!(shaping, [(100.0, 3.0, 1.0), (1000.0, -2.0, 1.0), (3500.0, 4.0, 1.0), (10_000.0, 1.0, 1.0)]);
        assert!(preset.bands.windows(2).all(|pair| pair[0].freq < pair[1].freq), "still low to high");

        let named: NamedEq = serde_json::from_str(r#"{ "name": "Mine", "gains_db": [1.0, 1.0, 1.0, 1.0, 1.0] }"#).unwrap();
        assert_eq!(named.bands.iter().filter(|band| band.shapes()).count(), 5);
    }

    #[test]
    fn an_eq_saves_and_loads_as_it_was() {
        let mut preset = EqPreset { enabled: false, ..EqPreset::default() };
        preset.bands[0] = EqBand { kind: EqKind::LowCut, freq: 40.0, gain_db: 0.0, q: 0.7, on: true };
        let json = serde_json::to_string(&preset).unwrap();
        assert_eq!(serde_json::from_str::<EqPreset>(&json).unwrap(), preset);
        let named = NamedEq { name: "Mine".into(), bands: preset.bands };
        assert_eq!(serde_json::from_str::<NamedEq>(&serde_json::to_string(&named).unwrap()).unwrap(), named);
    }

    #[test]
    fn there_are_no_tags_until_you_make_them() {
        assert!(ClipTags::default().tags.is_empty());
    }

    #[test]
    fn tags_go_on_and_off_a_clip_and_show_in_your_order() {
        let clip = Path::new(r"E:\Clips\Deadlock\Sonora 2026-10-28 00-41-10.mp4");
        let mut tags = funny_and_cool();
        tags.set(clip, "Cool", true);
        tags.set(clip, "Funny", true);
        assert_eq!(names(tags.of(clip)), ["Funny", "Cool"]);
        assert!(tags.has(clip, "Cool"));
        tags.set(clip, "Cool", false);
        tags.set(clip, "Funny", false);
        assert!(tags.of(clip).is_empty());
        assert!(tags.clips.is_empty(), "an untagged clip isn't kept");
    }

    #[test]
    fn a_tag_is_made_once_whatever_its_case() {
        let mut tags = funny_and_cool();
        assert_eq!(tags.add_tag("  Clutch ", RED), Ok("Clutch".to_string()));
        assert_eq!(tags.add_tag("funny", BLUE), Err(TagError::Exists("Funny".into())));
        assert_eq!(tags.add_tag("   ", RED), Err(TagError::Empty));
        assert_eq!(tags.add_tag(&"x".repeat(ClipTags::MAX_LEN + 1), RED), Err(TagError::TooLong));
        assert_eq!(names(&tags.tags), ["Funny", "Cool", "Clutch"]);
        assert_eq!(tags.tags[2].color, RED);
    }

    #[test]
    fn a_renamed_tag_stays_on_its_clips() {
        let clip = Path::new("one.mp4");
        let mut tags = funny_and_cool();
        tags.set(clip, "Funny", true);
        assert_eq!(tags.rename_tag("Funny", "Cool"), Err(TagError::Exists("Cool".into())));
        assert_eq!(tags.rename_tag("Funny", " funny "), Ok("funny".to_string()), "a new case is fine");
        assert_eq!(tags.rename_tag("funny", "Hilarious"), Ok("Hilarious".to_string()));
        assert_eq!(names(tags.of(clip)), ["Hilarious"]);
        assert_eq!(tags.tags[0].color, RED);
        tags.set_color("Hilarious", BLUE);
        assert_eq!(tags.tags[0].color, BLUE);
    }

    #[test]
    fn a_clip_can_be_said_to_be_from_a_game_and_back() {
        let clip = Path::new("unknown_replay_2024.05.16-20.28.mp4");
        let mut tags = ClipTags::default();
        tags.set_game(clip, Some("  Team Fortress 2 "));
        assert_eq!(tags.game_of(clip), Some("Team Fortress 2"));
        tags.set_game(clip, Some("   "));
        assert_eq!(tags.game_of(clip), None, "nothing typed goes back to what Sonora found");
    }

    #[test]
    fn deleting_a_tag_takes_it_off_every_clip() {
        let (one, two) = (Path::new("one.mp4"), Path::new("two.mp4"));
        let mut tags = funny_and_cool();
        tags.set(one, "Funny", true);
        tags.set(two, "Funny", true);
        tags.set(two, "Cool", true);
        tags.remove_tag("Funny");
        assert_eq!(names(&tags.tags), ["Cool"]);
        assert!(!tags.clips.contains_key(one));
        assert_eq!(names(tags.of(two)), ["Cool"]);
    }

    #[test]
    fn an_unreadable_tags_file_is_kept_and_tags_start_over() {
        let dir = test_dir("tags");
        let path = dir.join("clip-tags.json");
        assert_eq!(clip_tags_from(&path), ClipTags::default());
        std::fs::write(&path, "{ truncated").unwrap();
        assert_eq!(clip_tags_from(&path), ClipTags::default());
        assert!(files_in(&dir).iter().any(|f| f.contains("corrupt")));
    }
}
