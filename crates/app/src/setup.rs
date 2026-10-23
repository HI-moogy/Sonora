//! Restores the mixer as it was left (master and bus levels, output
//! devices, EQ, colors, and which apps and microphone feed which bus) and
//! saves it to `config.json` as it changes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use audio_engine::{process_executable_name, BusInfo, Mixer, SourceInfo, EQ_BAND_COUNT};
use common::{EqPreset, MicConfig, MixerSetup, SavedBus, SavedSource};
use eframe::egui;

use crate::AvailableSession;

/// How often changes are checked for and saved, and saved routes are
/// matched against running apps.
const CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How often to retry a saved microphone that couldn't be opened.
const MIC_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// What gets saved: the mixer, EQ and microphone sections.
#[derive(PartialEq)]
struct Snapshot {
    mixer: MixerSetup,
    eq: HashMap<String, EqPreset>,
    mic: MicConfig,
}

pub struct MixerPersistence {
    /// Saved routes that aren't running yet (the app isn't open, or the
    /// microphone couldn't be opened). Restored when possible, and kept in
    /// saves meanwhile so they aren't forgotten.
    pending: Vec<SavedSource>,
    /// Bus colors from the settings file, kept as written.
    colors: HashMap<String, String>,
    /// Lowercase executable name per app source id.
    exe_names: HashMap<u64, String>,
    saved: Option<Snapshot>,
    last_check: Instant,
    last_mic_try: Instant,
}

impl MixerPersistence {
    /// Applies the saved setup to `mixer`. Apps are routed as they appear
    /// (see [`Self::tick`]).
    pub fn restore(mixer: &Mixer) -> Self {
        let presets = common::load_presets();
        let setup = presets.mixer;
        mixer.set_master_gain(setup.master_gain);
        for (name, bus) in &setup.buses {
            mixer.ensure_bus(name);
            mixer.set_bus_gain(name, bus.gain);
            mixer.set_bus_muted(name, bus.muted);
            mixer.set_bus_device(name, bus.device.clone());
            mixer.set_bus_fallback(name, bus.fallback_to_default);
            if let Some(heard) = bus.heard {
                mixer.set_bus_heard(name, heard);
            }
            if let Some(priority) = bus.priority {
                mixer.set_bus_priority(name, priority);
            }
            mixer.set_bus_monitor(name, bus.monitor.unwrap_or(false));
        }
        for (name, preset) in &presets.eq {
            mixer.ensure_bus(name);
            for (band, &gain_db) in preset.gains_db.iter().enumerate().take(EQ_BAND_COUNT) {
                mixer.set_bus_eq_band(name, band, gain_db);
            }
        }
        mixer.mic_cleanup().set_mode(presets.mic.noise_suppression);
        mixer.mic_cleanup().set_quiet_far_voices(presets.mic.quiet_far_voices);
        let colors = setup.buses.iter().filter_map(|(name, bus)| Some((name.clone(), bus.color.clone()?))).collect();
        let mut persistence = Self {
            pending: setup.sources,
            colors,
            exe_names: HashMap::new(),
            saved: None,
            last_check: Instant::now() - CHECK_INTERVAL,
            last_mic_try: Instant::now() - MIC_RETRY_INTERVAL,
        };
        persistence.restore_mic(mixer);
        persistence
    }

    /// The bus's saved custom color, if it has a valid one.
    pub fn bus_color(&self, bus: &str) -> Option<egui::Color32> {
        parse_hex_color(self.colors.get(bus)?)
    }

    /// Saved app routes for `bus` whose app isn't running.
    pub fn waiting_apps<'a>(&'a self, bus: &'a str) -> impl Iterator<Item = &'a str> {
        self.pending.iter().filter(move |s| s.bus == bus).filter_map(|s| s.app.as_deref())
    }

    /// Forgets a saved route that's waiting for its app.
    pub fn forget_waiting_app(&mut self, bus: &str, app: &str) {
        self.pending.retain(|s| !(s.bus == bus && s.app.as_deref() == Some(app)));
    }

    /// Call every frame with the frame's mixer snapshots and the apps
    /// currently playing audio: routes saved apps that have
    /// started, and saves the setup once a second if it changed.
    pub fn tick(&mut self, mixer: &Mixer, buses: &[BusInfo], sources: &[SourceInfo], sessions: &[AvailableSession]) {
        if self.last_check.elapsed() < CHECK_INTERVAL {
            return;
        }
        self.last_check = Instant::now();
        self.restore_mic(mixer);
        self.restore_apps(mixer, sources, sessions);
        self.save_if_changed(mixer, buses, sources);
    }

    /// Saves now if anything changed (e.g. on exit).
    pub fn save_now(&mut self, mixer: &Mixer) {
        self.save_if_changed(mixer, &mixer.buses(), &mixer.sources());
    }

    fn restore_mic(&mut self, mixer: &Mixer) {
        let Some(index) = self.pending.iter().position(|s| s.app.is_none()) else { return };
        if self.last_mic_try.elapsed() < MIC_RETRY_INTERVAL {
            return;
        }
        self.last_mic_try = Instant::now();
        let saved = &self.pending[index];
        if let Ok(id) = mixer.add_mic_source("Microphone", saved.bus.clone(), saved.input_device.clone()) {
            mixer.set_source_gain(id, saved.gain);
            mixer.set_source_muted(id, saved.muted);
            self.pending.remove(index);
        }
    }

    fn restore_apps(&mut self, mixer: &Mixer, sources: &[SourceInfo], sessions: &[AvailableSession]) {
        if !self.pending.iter().any(|s| s.app.is_some()) {
            return;
        }
        for AvailableSession { pid, name } in sessions {
            if sources.iter().any(|s| s.pid == Some(*pid)) {
                continue;
            }
            let Some(exe) = process_executable_name(*pid).map(|e| e.to_lowercase()) else { continue };
            let Some(index) = self.pending.iter().position(|s| s.app.as_deref() == Some(exe.as_str())) else { continue };
            let saved = &self.pending[index];
            if let Ok(id) = mixer.add_source(*pid, name.clone(), saved.bus.clone()) {
                mixer.set_source_gain(id, saved.gain);
                mixer.set_source_muted(id, saved.muted);
                self.exe_names.insert(id, exe);
                self.pending.remove(index);
            }
        }
    }

    fn save_if_changed(&mut self, mixer: &Mixer, buses: &[BusInfo], sources: &[SourceInfo]) {
        let cleanup = mixer.mic_cleanup();
        let mic = MicConfig { noise_suppression: cleanup.mode(), quiet_far_voices: cleanup.quiet_far_voices() };
        let snapshot = self.snapshot(mixer.master_gain(), buses, sources, mic);
        if self.saved.as_ref() == Some(&snapshot) {
            return;
        }
        let Snapshot { mixer, eq, mic } = &snapshot;
        let result = common::update_presets(|presets| {
            presets.mixer = mixer.clone();
            presets.eq = eq.clone();
            presets.mic = *mic;
        });
        // On failure, try again on the next check.
        if result.is_ok() {
            self.saved = Some(snapshot);
        }
    }

    fn snapshot(&mut self, master_gain: f32, buses: &[BusInfo], sources: &[SourceInfo], mic: MicConfig) -> Snapshot {
        self.exe_names.retain(|id, _| sources.iter().any(|s| s.id == *id));
        let mut saved_sources: Vec<SavedSource> = sources
            .iter()
            .filter_map(|source| {
                let app = match source.pid {
                    Some(pid) => Some(match self.exe_names.get(&source.id) {
                        Some(exe) => exe.clone(),
                        None => {
                            let exe = process_executable_name(pid)?.to_lowercase();
                            self.exe_names.insert(source.id, exe.clone());
                            exe
                        }
                    }),
                    None => None,
                };
                Some(SavedSource {
                    app,
                    bus: source.bus.clone(),
                    gain: source.gain,
                    muted: source.muted,
                    input_device: source.input_device.clone(),
                })
            })
            .chain(self.pending.iter().cloned())
            .collect();
        saved_sources.sort_by(|a, b| (&a.bus, &a.app).cmp(&(&b.bus, &b.app)));

        let buses_saved = buses
            .iter()
            .map(|bus| {
                let saved = SavedBus {
                    gain: bus.gain,
                    muted: bus.muted,
                    device: bus.device_id.clone(),
                    fallback_to_default: bus.fallback_to_default,
                    heard: Some(bus.heard),
                    priority: Some(bus.priority),
                    monitor: Some(bus.monitor),
                    color: self.colors.get(&bus.name).cloned(),
                };
                (bus.name.clone(), saved)
            })
            .collect();
        let eq = buses.iter().map(|bus| (bus.name.clone(), EqPreset { gains_db: bus.eq_gains_db })).collect();
        Snapshot {
            mixer: MixerSetup { master_gain, buses: buses_saved, sources: saved_sources },
            eq,
            mic,
        }
    }
}

/// `#RRGGBB` (the `#` optional).
fn parse_hex_color(hex: &str) -> Option<egui::Color32> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(egui::Color32::from_rgb(channel(0)?, channel(2)?, channel(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_parse_with_or_without_hash() {
        assert_eq!(parse_hex_color("#FF6000"), Some(egui::Color32::from_rgb(0xFF, 0x60, 0x00)));
        assert_eq!(parse_hex_color("5865f2"), Some(egui::Color32::from_rgb(0x58, 0x65, 0xF2)));
        assert_eq!(parse_hex_color("#FF60"), None);
        assert_eq!(parse_hex_color("#GG6000"), None);
        assert_eq!(parse_hex_color("#ÿÿÿ"), None);
    }
}
