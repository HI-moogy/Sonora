//! Restores the mixer as it was left (master and bus levels, output
//! devices, EQ, colors, and which apps and microphone feed which bus) and
//! saves it to `config.json` as it changes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use audio_engine::{process_executable_name, BusInfo, Mixer, SourceInfo, EQ_BAND_COUNT};
use common::{EqPreset, MicConfig, MixerSetup, SavedBus, SavedSource};

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

/// Why a program typed in by name wasn't added.
#[derive(Debug, PartialEq)]
pub enum AddAppError {
    /// What was typed isn't a program's file name.
    NotAProgram,
    /// It's already in a channel (this one, or the one named).
    AlreadyIn(String),
}

/// The saved name (lowercase executable name, as apps are saved) of the
/// program `typed` names: "Deadlock", "deadlock.exe" or a path to it.
pub fn app_key(typed: &str) -> Option<String> {
    let path = typed.trim().trim_matches('"');
    let name = path.rsplit(['\\', '/']).next()?.trim().to_lowercase();
    if name.is_empty() || name.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return None;
    }
    // Windows programs are always .exe; dots elsewhere are part of the
    // name (Battle.net.exe).
    if cfg!(windows) && !name.ends_with(".exe") {
        return Some(format!("{name}.exe"));
    }
    Some(name)
}

pub struct MixerPersistence {
    /// Saved routes that aren't running yet (the app isn't open, or the
    /// microphone couldn't be opened). Restored when possible, and kept in
    /// saves meanwhile so they aren't forgotten.
    pending: Vec<SavedSource>,
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
            mixer.set_bus_surround(name, bus.surround);
        }
        for (name, preset) in &presets.eq {
            mixer.ensure_bus(name);
            for (band, &gain_db) in preset.gains_db.iter().enumerate().take(EQ_BAND_COUNT) {
                mixer.set_bus_eq_band(name, band, gain_db);
            }
        }
        mixer.mic_cleanup().set_mode(presets.mic.noise_suppression);
        mixer.mic_cleanup().set_quiet_far_voices(presets.mic.quiet_far_voices);
        let mut persistence = Self {
            pending: setup.sources,
            exe_names: HashMap::new(),
            saved: None,
            last_check: Instant::now() - CHECK_INTERVAL,
            last_mic_try: Instant::now() - MIC_RETRY_INTERVAL,
        };
        persistence.restore_mic(mixer, &[]);
        persistence
    }

    /// Saved app routes for `bus` whose app isn't running.
    pub fn waiting_apps<'a>(&'a self, bus: &'a str) -> impl Iterator<Item = &'a str> {
        self.pending.iter().filter(move |s| s.bus == bus).filter_map(|s| s.app.as_deref())
    }

    /// Forgets a saved route that's waiting for its app.
    pub fn forget_waiting_app(&mut self, bus: &str, app: &str) {
        self.pending.retain(|s| !(s.bus == bus && s.app.as_deref() == Some(app)));
    }

    /// The app (lowercase executable name) running source `id` is, once
    /// known (within a check of it starting).
    pub fn app_of(&self, id: u64) -> Option<&str> {
        self.exe_names.get(&id).map(String::as_str)
    }

    /// Saves the program `typed` names (as typed, e.g. "Deadlock", or the
    /// path to it) to join `bus` whenever it plays, as if added from the
    /// list. Returns the name saved.
    pub fn add_app_by_name(&mut self, bus: &str, typed: &str, sources: &[SourceInfo]) -> Result<String, AddAppError> {
        let app = app_key(typed).ok_or(AddAppError::NotAProgram)?;
        let running = sources.iter().filter_map(|source| {
            let exe = match self.exe_names.get(&source.id) {
                Some(exe) => exe.clone(),
                None => process_executable_name(source.pid?)?.to_lowercase(),
            };
            Some((exe, source.bus.clone()))
        });
        let waiting = self.pending.iter().filter_map(|saved| Some((saved.app.clone()?, saved.bus.clone())));
        if let Some((_, channel)) = running.chain(waiting).find(|(exe, _)| *exe == app) {
            return Err(AddAppError::AlreadyIn(channel));
        }
        self.pending.push(SavedSource { app: Some(app.clone()), bus: bus.to_string(), gain: 1.0, muted: false, input_device: None });
        Ok(app)
    }

    /// Call often (every frame is fine) with the apps currently playing
    /// audio: once a second, routes saved apps that have started, and saves
    /// the setup if it changed.
    pub fn tick(&mut self, mixer: &Mixer, sessions: &[AvailableSession]) {
        if self.last_check.elapsed() < CHECK_INTERVAL {
            return;
        }
        self.last_check = Instant::now();
        let sources = mixer.source_settings();
        self.restore_mic(mixer, &sources);
        self.restore_apps(mixer, &sources, sessions);
        self.save_now(mixer);
    }

    /// Saves now if anything changed (e.g. on exit).
    pub fn save_now(&mut self, mixer: &Mixer) {
        self.save_if_changed(mixer, &mixer.bus_settings(), &mixer.source_settings());
    }

    /// Retries the saved microphone until it opens. Sonora records one
    /// microphone, so once one is running (this, or one picked by hand while
    /// this couldn't open) the saved one is done with; settings saved by
    /// versions that recorded two keep only the first.
    fn restore_mic(&mut self, mixer: &Mixer, sources: &[SourceInfo]) {
        if sources.iter().any(|s| s.pid.is_none()) {
            self.pending.retain(|s| s.app.is_some());
            return;
        }
        let Some(index) = self.pending.iter().position(|s| s.app.is_none()) else { return };
        if self.last_mic_try.elapsed() < MIC_RETRY_INTERVAL {
            return;
        }
        self.last_mic_try = Instant::now();
        let saved = &self.pending[index];
        if let Ok(id) = mixer.add_mic_source("Microphone", saved.bus.clone(), saved.input_device.clone()) {
            mixer.set_source_gain(id, saved.gain);
            mixer.set_source_muted(id, saved.muted);
            self.pending.retain(|s| s.app.is_some());
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
                    surround: bus.surround,
                    color: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn persistence(pending: Vec<SavedSource>) -> MixerPersistence {
        MixerPersistence {
            pending,
            exe_names: HashMap::new(),
            saved: None,
            last_check: Instant::now(),
            last_mic_try: Instant::now(),
        }
    }

    fn waiting(app: &str, bus: &str) -> SavedSource {
        SavedSource { app: Some(app.into()), bus: bus.into(), gain: 1.0, muted: false, input_device: None }
    }

    fn running(id: u64, bus: &str) -> SourceInfo {
        SourceInfo {
            id,
            pid: None,
            name: String::new(),
            bus: bus.into(),
            gain: 1.0,
            muted: false,
            level: 0.0,
            input_device: None,
            direct: false,
        }
    }

    #[test]
    #[cfg(windows)]
    fn typed_programs_are_saved_by_their_lowercase_exe_name() {
        for typed in ["deadlock.exe", "Deadlock.EXE", " Deadlock ", r#""C:\Games\Deadlock\game\bin\win64\deadlock.exe""#, "D:/Games/deadlock.exe"] {
            assert_eq!(app_key(typed).as_deref(), Some("deadlock.exe"), "{typed}");
        }
        assert_eq!(app_key("Battle.net").as_deref(), Some("battle.net.exe"));
    }

    #[test]
    fn what_isnt_a_program_name_is_refused() {
        for typed in ["", "   ", r"C:\Games\", "dead|lock", "C:"] {
            assert_eq!(app_key(typed), None, "{typed:?}");
        }
    }

    #[test]
    fn a_program_added_by_name_waits_for_it_and_is_saved() {
        let app = app_key("deadlock").unwrap();
        let mut saved = persistence(Vec::new());
        assert_eq!(saved.add_app_by_name("Game", "Deadlock", &[]), Ok(app.clone()));
        assert_eq!(saved.waiting_apps("Game").collect::<Vec<_>>(), [app.as_str()]);

        // Saved like any routed app, and removable again.
        let snapshot = saved.snapshot(1.0, &[], &[], MicConfig::default());
        assert_eq!(snapshot.mixer.sources, [waiting(&app, "Game")]);
        saved.forget_waiting_app("Game", &app);
        assert_eq!(saved.waiting_apps("Game").count(), 0);
    }

    #[test]
    fn a_program_already_in_a_channel_isnt_added_again() {
        let app = app_key("deadlock").unwrap();
        let mut saved = persistence(vec![waiting(&app, "Chat")]);
        assert_eq!(saved.add_app_by_name("Game", "deadlock", &[]), Err(AddAppError::AlreadyIn("Chat".into())));

        let mut saved = persistence(Vec::new());
        saved.exe_names.insert(7, app.clone());
        assert_eq!(saved.add_app_by_name("Game", "Deadlock", &[running(7, "Game")]), Err(AddAppError::AlreadyIn("Game".into())));
        assert_eq!(saved.waiting_apps("Game").count(), 0);
    }

    #[test]
    fn a_program_name_that_isnt_one_is_not_saved() {
        let mut saved = persistence(Vec::new());
        assert_eq!(saved.add_app_by_name("Game", "  ", &[]), Err(AddAppError::NotAProgram));
        assert!(saved.pending.is_empty());
    }
}
