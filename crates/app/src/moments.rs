use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio_engine::{BusTap, Mixer};
use capture_engine::{default_clips_dir, AudioInput, ClipSaver, Recorder, Resolution, VideoSettings};
use common::MomentsConfig;
use eframe::egui;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONHAND, MB_OK};

use crate::clips::{self, ClipList};
use crate::hotkey::{self, GlobalHotkey};
use crate::FIXED_BUSES;

const CLIP_SCAN_INTERVAL: Duration = Duration::from_secs(5);
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 100, 100);

/// Feeds every mixer bus into the clip recorder's track of the same name.
struct BusesToRecorder(AudioInput);

impl BusTap for BusesToRecorder {
    fn bus_audio(&self, bus: &str, samples: &[f32]) {
        self.0.push(bus, samples);
    }
}

/// What the hotkey thread needs: it saves clips without going through the
/// UI, which may not be running while a game has focus.
#[derive(Default)]
struct Shared {
    saver: Mutex<Option<ClipSaver>>,
    dir: Mutex<PathBuf>,
    last_result: Mutex<Option<Result<PathBuf, String>>>,
}

impl Shared {
    fn save(self: &Arc<Self>, ctx: &egui::Context) {
        let Some(saver) = self.saver.lock().unwrap().clone() else {
            self.finish(ctx, Err("Moments is off".into()));
            return;
        };
        let reply = saver.save(self.dir.lock().unwrap().clone());
        let (shared, ctx) = (self.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = match reply.recv() {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err("the recorder stopped".into()),
            };
            shared.finish(&ctx, result);
        });
    }

    /// Records the outcome and confirms it audibly, since you're usually in
    /// a game rather than looking at sonar-rs when you save.
    fn finish(&self, ctx: &egui::Context, result: Result<PathBuf, String>) {
        unsafe {
            let _ = MessageBeep(if result.is_ok() { MB_OK } else { MB_ICONHAND });
        }
        *self.last_result.lock().unwrap() = Some(result);
        ctx.request_repaint();
    }
}

pub struct Moments {
    config: MomentsConfig,
    recorder: Option<Recorder>,
    start_error: Option<String>,
    shared: Arc<Shared>,
    hotkey: Option<GlobalHotkey>,
    hotkey_error: Option<String>,
    capturing_hotkey: bool,
    clips: ClipList,
    last_scan: Option<Instant>,
    ctx: egui::Context,
}

impl Moments {
    pub fn new(ctx: &egui::Context, mixer: &Mixer) -> Self {
        let mut moments = Self {
            config: common::load_presets().moments,
            recorder: None,
            start_error: None,
            shared: Arc::default(),
            hotkey: None,
            hotkey_error: None,
            capturing_hotkey: false,
            clips: ClipList::default(),
            last_scan: None,
            ctx: ctx.clone(),
        };
        *moments.shared.dir.lock().unwrap() = moments.clips_dir();
        moments.restart_recorder(mixer);
        moments.register_hotkey();
        moments
    }

    fn clips_dir(&self) -> PathBuf {
        self.config.clips_dir.clone().unwrap_or_else(default_clips_dir)
    }

    fn video_settings(&self) -> VideoSettings {
        let resolution = Resolution::ALL
            .into_iter()
            .find(|r| r.size().1 == self.config.height)
            .unwrap_or(Resolution::P1080);
        VideoSettings { resolution, fps: self.config.fps, buffer_secs: self.config.buffer_secs }
    }

    fn track_volume(&self, bus: &str) -> f32 {
        self.config.track_volumes.get(bus).copied().unwrap_or(1.0)
    }

    fn restart_recorder(&mut self, mixer: &Mixer) {
        mixer.set_bus_tap(None);
        *self.shared.saver.lock().unwrap() = None;
        // Stop the old recorder before starting another: GPUs allow only a
        // few hardware encoder sessions at once.
        self.recorder = None;
        self.start_error = None;
        if !self.config.enabled {
            return;
        }
        match Recorder::start(self.video_settings(), FIXED_BUSES) {
            Ok(recorder) => {
                for bus in FIXED_BUSES {
                    recorder.set_track_volume(bus, self.track_volume(bus));
                }
                mixer.set_bus_tap(Some(Arc::new(BusesToRecorder(recorder.audio_input()))));
                *self.shared.saver.lock().unwrap() = Some(recorder.saver());
                self.recorder = Some(recorder);
            }
            Err(error) => self.start_error = Some(error.to_string()),
        }
    }

    fn register_hotkey(&mut self) {
        self.hotkey = None;
        let (shared, ctx) = (self.shared.clone(), self.ctx.clone());
        match GlobalHotkey::register(self.config.hotkey, move || shared.save(&ctx)) {
            Ok(registered) => {
                self.hotkey = Some(registered);
                self.hotkey_error = None;
            }
            Err(error) => self.hotkey_error = Some(error),
        }
    }

    fn persist(&self) {
        let moments = self.config.clone();
        let _ = common::update_presets(|presets| presets.moments = moments);
    }

    /// One-line status: what's buffered, or why nothing is.
    fn status(&self) -> (String, bool) {
        if !self.config.enabled {
            return ("Off".into(), false);
        }
        if let Some(error) = &self.start_error {
            return (format!("Couldn't start: {error}"), true);
        }
        let Some(recorder) = &self.recorder else {
            return ("Not running".into(), true);
        };
        let status = recorder.status();
        if let Some(error) = status.error() {
            return (format!("Stopped: {error}"), true);
        }
        let buffered = status.buffered().min(Duration::from_secs(self.config.buffer_secs as u64));
        (
            format!(
                "Buffering {} of {} · {}",
                clips::format_duration(buffered),
                clips::format_duration(Duration::from_secs(self.config.buffer_secs as u64)),
                clips::format_size(status.buffered_bytes() as u64),
            ),
            false,
        )
    }

    /// Rough memory the buffer needs at full length with these settings.
    fn estimated_buffer_bytes(&self) -> u64 {
        let audio_bits = 160_000 * (FIXED_BUSES.len() as u64 + 1);
        (self.video_settings().bitrate() as u64 + audio_bits) / 8 * self.config.buffer_secs as u64
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, mixer: &Mixer) {
        let dir = self.clips_dir();
        if self.last_scan.is_none_or(|at| at.elapsed() >= CLIP_SCAN_INTERVAL) {
            self.clips.refresh(&dir);
            self.last_scan = Some(Instant::now());
        }

        ui.horizontal(|ui| {
            let mut enabled = self.config.enabled;
            if ui.checkbox(&mut enabled, egui::RichText::new("Record Moments").strong()).changed() {
                self.config.enabled = enabled;
                self.persist();
                self.restart_recorder(mixer);
            }
            ui.separator();
            let (text, is_error) = self.status();
            if is_error {
                ui.colored_label(ERROR_COLOR, text);
            } else {
                ui.label(text);
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(self.recorder.is_some(), egui::Button::new("Save clip")).clicked() {
                self.shared.save(ui.ctx());
            }
            ui.weak(format!("or press {} anywhere", hotkey::label(self.config.hotkey)));
        });
        let saved = self.shared.last_result.lock().unwrap().clone();
        match saved {
            Some(Ok(path)) => {
                ui.horizontal(|ui| {
                    ui.label(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                    if ui.small_button("Play").clicked() {
                        clips::play(&path);
                    }
                });
                // A new clip just landed; show it in the list right away.
                if self.clips.entries.first().is_none_or(|newest| newest.path != path) {
                    self.last_scan = None;
                }
            }
            Some(Err(error)) => {
                ui.colored_label(ERROR_COLOR, format!("Couldn't save clip: {error}"));
            }
            None => {}
        }
        ui.separator();

        egui::Grid::new("moments-settings").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Clip length");
            let mut secs = self.config.buffer_secs;
            let response = ui.add(
                egui::Slider::new(&mut secs, 10..=300)
                    .step_by(5.0)
                    .custom_formatter(|v, _| clips::format_duration(Duration::from_secs(v as u64))),
            );
            self.config.buffer_secs = secs;
            // Restarting clears the buffer, so do it once the slider is let go.
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                self.persist();
                self.restart_recorder(mixer);
            }
            ui.end_row();

            ui.label("Quality");
            ui.horizontal(|ui| {
                let mut changed = false;
                let current = self.video_settings().resolution;
                egui::ComboBox::from_id_salt("moments-resolution").selected_text(current.label()).show_ui(ui, |ui| {
                    for resolution in Resolution::ALL {
                        if ui.selectable_label(resolution == current, resolution.label()).clicked() {
                            self.config.height = resolution.size().1;
                            changed = true;
                        }
                    }
                });
                egui::ComboBox::from_id_salt("moments-fps")
                    .selected_text(format!("{} fps", self.config.fps))
                    .show_ui(ui, |ui| {
                        for fps in [30, 60] {
                            if ui.selectable_label(self.config.fps == fps, format!("{fps} fps")).clicked() {
                                self.config.fps = fps;
                                changed = true;
                            }
                        }
                    });
                ui.weak(format!("~{} of memory", clips::format_size(self.estimated_buffer_bytes())));
                if changed {
                    self.persist();
                    self.restart_recorder(mixer);
                }
            });
            ui.end_row();

            ui.label("Hotkey");
            ui.horizontal(|ui| {
                if self.capturing_hotkey {
                    ui.label("Press the new key combination… (Esc to cancel)");
                    let pressed = ui.input(|input| {
                        input.events.iter().find_map(|event| match event {
                            egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                            _ => None,
                        })
                    });
                    if let Some((key, modifiers)) = pressed {
                        if key == egui::Key::Escape {
                            self.capturing_hotkey = false;
                            self.register_hotkey();
                        } else if let Some(new_hotkey) = hotkey::from_key_press(key, modifiers) {
                            self.config.hotkey = new_hotkey;
                            self.capturing_hotkey = false;
                            self.persist();
                            self.register_hotkey();
                        }
                    }
                } else {
                    ui.monospace(hotkey::label(self.config.hotkey));
                    if ui.button("Change").clicked() {
                        // Release the old hotkey so pressing it again can be
                        // captured here instead of saving a clip.
                        self.hotkey = None;
                        self.capturing_hotkey = true;
                    }
                }
            });
            ui.end_row();
            if let Some(error) = &self.hotkey_error {
                ui.label("");
                ui.colored_label(ERROR_COLOR, error);
                ui.end_row();
            }

            ui.label("Clips folder");
            ui.horizontal(|ui| {
                ui.label(dir.display().to_string());
                if ui.button("Change…").clicked() {
                    if let Some(picked) = clips::pick_folder(&dir) {
                        self.config.clips_dir = Some(picked.clone());
                        *self.shared.dir.lock().unwrap() = picked;
                        self.persist();
                        self.last_scan = None;
                    }
                }
                if ui.button("Open").clicked() {
                    clips::open_folder(&dir);
                }
            });
            ui.end_row();
        });

        ui.separator();
        ui.strong("Clip audio");
        ui.weak("How loud each track is in saved clips. This doesn't change what you hear.");
        egui::Grid::new("moments-tracks").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            for bus in FIXED_BUSES {
                ui.label(*bus);
                let mut volume = self.track_volume(bus);
                let response = ui.add(
                    egui::Slider::new(&mut volume, 0.0..=2.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                if response.changed() {
                    self.config.track_volumes.insert(bus.to_string(), volume);
                    if let Some(recorder) = &self.recorder {
                        recorder.set_track_volume(bus, volume);
                    }
                }
                if response.drag_stopped() || (response.changed() && !response.dragged()) {
                    self.persist();
                }
                ui.end_row();
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(format!("Clips ({})", self.clips.entries.len()));
            if ui.small_button("Refresh").clicked() {
                self.last_scan = None;
            }
        });
        if self.clips.entries.is_empty() {
            ui.weak(format!(
                "No clips yet. Press {} right after something happens to save the last {}.",
                hotkey::label(self.config.hotkey),
                clips::format_duration(Duration::from_secs(self.config.buffer_secs as u64)),
            ));
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("moments-clips").num_columns(5).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                for clip in &self.clips.entries {
                    ui.label(&clip.name);
                    ui.label(clip.duration.map(clips::format_duration).unwrap_or_else(|| "?".into()));
                    ui.label(clips::format_size(clip.size));
                    if ui.small_button("Play").clicked() {
                        clips::play(&clip.path);
                    }
                    if ui.small_button("Show in folder").clicked() {
                        clips::show_in_folder(&clip.path);
                    }
                    ui.end_row();
                }
            });
        });
    }
}
