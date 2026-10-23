use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio_engine::{BusTap, Mixer};
use capture_engine::{
    default_clips_dir, AudioInput, CaptureTarget, ClipSaver, Recorder, Resolution, VideoSettings, DESKTOP_TRACK,
};
use common::{ClipSort, MomentsConfig, RecordMode};
use eframe::egui;

use crate::clips::editor::{ClipEditor, EditorAction};
use crate::clips::{self, ClipEntry, ClipList, Thumbnails};
use crate::games::{self, AppWindow, GameFinder};
use crate::hotkey::{self, GlobalHotkey};
use crate::{sounds, theme, FIXED_BUSES};

/// How often the clips folder (and its subfolders) is looked through again.
const CLIP_SCAN_INTERVAL: Duration = Duration::from_secs(10);
/// How often to look for games (and which one is in front) to record.
const GAME_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// On-screen size of a clip's thumbnail in the Clips tab.
const CARD_THUMBNAIL: egui::Vec2 = egui::vec2(256.0, 144.0);

/// Every audio track a clip has, in file order: one per bus, then desktop
/// audio (the default track).
fn clip_tracks() -> impl Iterator<Item = &'static str> {
    FIXED_BUSES.iter().copied().chain([DESKTOP_TRACK])
}
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
    /// The game being recorded, if any: its clips go in a folder named
    /// after it, like other recorders do.
    game: Mutex<Option<String>>,
    last_result: Mutex<Option<Result<PathBuf, String>>>,
}

impl Shared {
    fn save(self: &Arc<Self>, ctx: &egui::Context) {
        let Some(saver) = self.saver.lock().unwrap().clone() else {
            self.finish(ctx, Err("Moments is off".into()));
            return;
        };
        let mut dir = self.dir.lock().unwrap().clone();
        if let Some(game) = self.game.lock().unwrap().as_deref() {
            dir.push(game);
        }
        let reply = saver.save(dir);
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
    /// a game rather than looking at Sonora when you save.
    fn finish(&self, ctx: &egui::Context, result: Result<PathBuf, String>) {
        if result.is_ok() {
            sounds::clip_saved();
        } else {
            sounds::clip_failed();
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
    thumbnails: Thumbnails,
    last_scan: Option<Instant>,
    show_settings: bool,
    /// The clip open in the editor, shown instead of the clip list.
    editor: Option<ClipEditor>,
    /// Show only this game's clips (a [`clips::game_key`]).
    game_filter: Option<String>,
    ctx: egui::Context,
    games: GameFinder,
    /// Running apps' windows as of the last check.
    apps: Vec<AppWindow>,
    /// What the recorder was last told to capture.
    target: CaptureTarget,
    last_game_check: Option<Instant>,
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
            clips: ClipList::new(ctx),
            thumbnails: Thumbnails::new(ctx),
            last_scan: None,
            show_settings: false,
            editor: None,
            game_filter: None,
            ctx: ctx.clone(),
            games: GameFinder::new(),
            apps: Vec::new(),
            target: CaptureTarget::Screen,
            last_game_check: None,
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
                for track in clip_tracks() {
                    recorder.set_track_volume(track, self.track_volume(track));
                }
                mixer.set_bus_tap(Some(Arc::new(BusesToRecorder(recorder.audio_input()))));
                *self.shared.saver.lock().unwrap() = Some(recorder.saver());
                self.recorder = Some(recorder);
                // A new recorder starts on the whole screen; switch now
                // rather than recording the desktop until the next check.
                self.target = CaptureTarget::Screen;
                self.last_game_check = None;
                self.tick();
            }
            Err(error) => self.start_error = Some(error.to_string()),
        }
    }

    /// Keeps the recorder on the right target: the whole screen, or the
    /// game (or chosen app) in front. Runs every frame, also while the
    /// window is hidden in the tray, and checks about once a second.
    pub fn tick(&mut self) {
        if self.last_game_check.is_some_and(|at| at.elapsed() < GAME_CHECK_INTERVAL) {
            return;
        }
        self.last_game_check = Some(Instant::now());
        let target = match self.config.record {
            Some(RecordMode::Games) => {
                self.apps = self.games.app_windows();
                let current = match self.target {
                    CaptureTarget::Window(window) => Some(window),
                    _ => None,
                };
                games::pick_window(&self.apps, &self.config.always_record, &self.config.never_record, current)
                    .map_or(CaptureTarget::Nothing, |app| CaptureTarget::Window(app.window))
            }
            Some(RecordMode::Screen) | None => CaptureTarget::Screen,
        };
        if target != self.target {
            if let Some(recorder) = &self.recorder {
                recorder.set_target(target);
            }
            self.target = target;
            *self.shared.game.lock().unwrap() = self.recording_app().and_then(|app| folder_name(&app.title));
        }
    }

    /// The app being recorded in games mode, if any.
    fn recording_app(&self) -> Option<&AppWindow> {
        match self.target {
            CaptureTarget::Window(window) => self.apps.iter().find(|app| app.window == window),
            _ => None,
        }
    }

    fn set_record_mode(&mut self, mode: RecordMode) {
        self.config.record = Some(mode);
        self.persist();
        self.last_game_check = None;
        self.tick();
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
        let what = match self.config.record {
            Some(RecordMode::Games) => match self.recording_app() {
                Some(app) => format!("Recording {} · ", app.title),
                None => "No game open, nothing being recorded · ".to_string(),
            },
            _ => String::new(),
        };
        (
            format!(
                "{what}Buffering {} of {} · {}",
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

    /// The Clips tab: clips fill the tab; the gear opens clip settings in a
    /// side panel.
    pub fn tab(&mut self, ui: &mut egui::Ui, mixer: &Mixer) {
        if self.show_settings {
            egui::Panel::right("clip-settings")
                .size_range(380.0..=480.0)
                .default_size(430.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::SURFACE)
                        .inner_margin(egui::Margin::same(16))
                        .stroke(egui::Stroke::new(1.0, theme::BORDER)),
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Clip settings");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("✕").frame(false)).on_hover_text("Close settings").clicked() {
                                self.show_settings = false;
                            }
                        });
                    });
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.settings_ui(ui, mixer));
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(20)))
            .show(ui, |ui| match &mut self.editor {
            Some(editor) => match editor.ui(ui) {
                EditorAction::None => {}
                EditorAction::Close => self.editor = None,
                EditorAction::Saved => self.last_scan = None,
            },
            None => self.clips_ui(ui),
        });
    }

    /// Clip settings, in stacked sections that fit the panel's width
    /// (long text wraps instead of pushing the panel wider).
    fn settings_ui(&mut self, ui: &mut egui::Ui, mixer: &Mixer) {
        theme::section(ui, "Recording");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut enabled = self.config.enabled;
            if ui.checkbox(&mut enabled, egui::RichText::new("Record clips").strong()).changed() {
                self.config.enabled = enabled;
                self.persist();
                self.restart_recorder(mixer);
            }
            let (text, is_error) = self.status();
            ui.label(egui::RichText::new(text).color(if is_error { ERROR_COLOR } else { theme::TEXT_MUTED }));
        });

        theme::section(ui, "What to record");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mode = self.config.record.unwrap_or(RecordMode::Screen);
            if ui.radio(mode == RecordMode::Screen, "Whole screen").clicked() {
                self.set_record_mode(RecordMode::Screen);
            }
            if ui.radio(mode == RecordMode::Games, "Only games and apps I choose").clicked() {
                self.set_record_mode(RecordMode::Games);
            }
        });
        if self.config.record == Some(RecordMode::Games) {
            theme::section(ui, "Games and apps");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.games_ui(ui);
            });
        }

        theme::section(ui, "Length and quality");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
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
            });
            ui.horizontal(|ui| {
                ui.label("Quality");
                let mut changed = false;
                let current = self.video_settings().resolution;
                egui::ComboBox::from_id_salt("moments-resolution")
                    .width(90.0)
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for resolution in Resolution::ALL {
                            if ui.selectable_label(resolution == current, resolution.label()).clicked() {
                                self.config.height = resolution.size().1;
                                changed = true;
                            }
                        }
                    });
                egui::ComboBox::from_id_salt("moments-fps")
                    .width(80.0)
                    .selected_text(format!("{} fps", self.config.fps))
                    .show_ui(ui, |ui| {
                        for fps in [30, 60] {
                            if ui.selectable_label(self.config.fps == fps, format!("{fps} fps")).clicked() {
                                self.config.fps = fps;
                                changed = true;
                            }
                        }
                    });
                if changed {
                    self.persist();
                    self.restart_recorder(mixer);
                }
            });
            ui.label(
                egui::RichText::new(format!("Uses about {} of memory.", clips::format_size(self.estimated_buffer_bytes())))
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        });

        theme::section(ui, "Hotkey");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
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
                ui.horizontal(|ui| {
                    ui.label("Save a clip with");
                    ui.label(egui::RichText::new(hotkey::label(self.config.hotkey)).monospace().strong());
                    if ui.button("Change").clicked() {
                        // Release the old hotkey so pressing it again can be
                        // captured here instead of saving a clip.
                        self.hotkey = None;
                        self.capturing_hotkey = true;
                    }
                });
            }
            if let Some(error) = &self.hotkey_error {
                ui.colored_label(ERROR_COLOR, error);
            }
        });

        theme::section(ui, "Clip audio");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(
                    "How loud each track is in saved clips; what you hear doesn't change. Desktop audio is \
                     everything you hear and plays by default; the channel tracks are for editing.",
                )
                .small()
                .color(theme::TEXT_MUTED),
            );
            egui::Grid::new("moments-tracks").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for bus in clip_tracks() {
                    ui.label(if bus == DESKTOP_TRACK { "Desktop audio" } else { bus });
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
        });
    }

    /// Which games and apps get recorded: what's open now, and the apps
    /// you added or left out.
    fn games_ui(&mut self, ui: &mut egui::Ui) {
        ui.weak(
            "Games are found automatically. Only the game's own window is recorded (not the rest of the screen), \
             and nothing while no game is open. The one in front is recorded.",
        );
        let mut changed = false;
        ui.label("Open now");
        let mut seen = std::collections::HashSet::new();
        let apps: Vec<AppWindow> = self.apps.iter().filter(|app| seen.insert(app.exe.clone())).cloned().collect();
        for app in &apps {
            let recording = self.recording_app().is_some_and(|r| r.exe == app.exe);
            let always = self.config.always_record.contains(&app.exe);
            let never = self.config.never_record.contains(&app.exe);
            let note = match (app.is_game, never, always, recording) {
                (_, _, _, true) => "recording now",
                (true, true, _, _) => "game, left out",
                (true, false, _, _) => "game",
                (false, _, true, _) => "added",
                (false, _, false, _) => "",
            };
            ui.horizontal(|ui| {
                let (text, action) = match (app.is_game, never, always) {
                    (true, true, _) => ("Record it", 0),
                    (true, false, _) => ("Don't record", 1),
                    (false, _, true) => ("Stop recording", 2),
                    (false, _, false) => ("Record it", 3),
                };
                // The button keeps its place on the right; the name (a
                // window title, possibly a browser tab's long one) gets the
                // rest and is cut short if needed.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(text).clicked() {
                        match action {
                            0 => self.config.never_record.retain(|exe| exe != &app.exe),
                            1 => self.config.never_record.push(app.exe.clone()),
                            2 => self.config.always_record.retain(|exe| exe != &app.exe),
                            _ => self.config.always_record.push(app.exe.clone()),
                        }
                        changed = true;
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.add(egui::Label::new(&app.title).truncate()).on_hover_text(&app.exe);
                        if !note.is_empty() {
                            let color = if recording { theme::ACCENT } else { theme::TEXT_MUTED };
                            ui.label(egui::RichText::new(note).small().color(color));
                        }
                    });
                });
            });
            ui.separator();
        }
        // Choices for apps that aren't open right now.
        let open: Vec<&str> = apps.iter().map(|app| app.exe.as_str()).collect();
        for (list, verb) in [(&mut self.config.always_record, "Recorded"), (&mut self.config.never_record, "Left out")] {
            let closed: Vec<String> = list.iter().filter(|exe| !open.contains(&exe.as_str())).cloned().collect();
            if closed.is_empty() {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{verb} when open:"));
                for exe in closed {
                    if ui.small_button(format!("{exe} ✕")).on_hover_text("Forget this choice").clicked() {
                        list.retain(|e| e != &exe);
                        changed = true;
                    }
                }
            });
        }
        if changed {
            self.persist();
            self.last_game_check = None;
            self.tick();
        }
    }

    /// Asks once what clips should record.
    fn record_mode_question(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("What should clips record?");
            ui.label(
                "The whole screen catches everything, including other apps and notifications. \
                 Games only records just the game's window, automatically whenever a game is open.",
            );
            ui.horizontal(|ui| {
                if ui.button("Whole screen").clicked() {
                    self.set_record_mode(RecordMode::Screen);
                }
                if ui.button("Only games and apps I choose").clicked() {
                    self.set_record_mode(RecordMode::Games);
                    self.show_settings = true;
                }
            });
            ui.weak("You can change this later in ⚙ settings.");
        });
    }

    /// Where clips are saved, the recording status, and every saved clip as
    /// a thumbnail you click to play.
    fn clips_ui(&mut self, ui: &mut egui::Ui) {
        let dir = self.clips_dir();
        let last_result = self.shared.last_result.lock().unwrap().clone();
        // A clip saved from the hotkey shows up right away, not on the next scan.
        self.clips.poll();
        if let Some(Ok(path)) = &last_result {
            if !self.clips.scanning && !self.clips.entries.iter().any(|clip| &clip.path == path) {
                self.last_scan = None;
            }
        }
        if self.last_scan.is_none_or(|at| at.elapsed() >= CLIP_SCAN_INTERVAL) {
            self.clips.refresh(&dir);
            self.last_scan = Some(Instant::now());
        }
        self.thumbnails.begin_frame(ui.ctx());

        ui.horizontal(|ui| {
            ui.heading("Clips");
            let total: u64 = self.clips.entries.iter().map(|clip| clip.size).sum();
            let count = self.clips.entries.len();
            ui.weak(format!("{count} clip{} · {}", if count == 1 { "" } else { "s" }, clips::format_size(total)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let gear = ui
                    .add(egui::Button::new(egui::RichText::new("⚙").size(18.0)).selected(self.show_settings))
                    .on_hover_text("Clip settings");
                if gear.clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.add_enabled(self.recorder.is_some(), egui::Button::new("Save clip")).clicked() {
                    self.shared.save(ui.ctx());
                }
            });
        });
        if self.config.record.is_none() && self.recorder.is_some() {
            self.record_mode_question(ui);
        }
        let (status, is_error) = self.status();
        if is_error {
            ui.colored_label(ERROR_COLOR, status);
        } else if self.config.record == Some(RecordMode::Games) && self.recorder.is_some() {
            ui.weak(match self.recording_app() {
                Some(app) => format!(
                    "Recording {} · press {} anywhere to save the last {}",
                    app.title,
                    hotkey::label(self.config.hotkey),
                    clips::format_duration(Duration::from_secs(self.config.buffer_secs as u64)),
                ),
                None => "No game open, so nothing is being recorded. Open a game to start.".to_string(),
            });
        } else if self.recorder.is_some() {
            ui.weak(format!(
                "Recording the last {} · press {} anywhere to save it",
                clips::format_duration(Duration::from_secs(self.config.buffer_secs as u64)),
                hotkey::label(self.config.hotkey),
            ));
        } else {
            ui.weak("Recording is off. Turn it on in ⚙ settings.");
        }
        if let Some(Err(error)) = &last_result {
            ui.colored_label(ERROR_COLOR, format!("Couldn't save the last clip: {error}"));
        }
        ui.horizontal(|ui| {
            ui.label("Saved to");
            ui.strong(dir.display().to_string());
            if ui.button("Open folder").clicked() {
                clips::open_folder(&dir);
            }
            if ui.button("Change folder…").clicked() {
                if let Some(picked) = clips::pick_folder(&dir) {
                    self.config.clips_dir = Some(picked.clone());
                    *self.shared.dir.lock().unwrap() = picked;
                    self.persist();
                    self.last_scan = None;
                }
            }
            if self.config.clips_dir.is_some() && ui.button("Use default").clicked() {
                self.config.clips_dir = None;
                *self.shared.dir.lock().unwrap() = default_clips_dir();
                self.persist();
                self.last_scan = None;
            }
        });
        ui.separator();

        if self.clips.entries.is_empty() {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(if self.clips.scanned {
                format!(
                    "No clips yet. Press {} right after something happens to save the last {}.",
                    hotkey::label(self.config.hotkey),
                    clips::format_duration(Duration::from_secs(self.config.buffer_secs as u64)),
                )
            } else {
                "Looking for clips…".to_string()
            })
            .color(theme::TEXT_MUTED));
            return;
        }

        // Each game with how many clips it has, for the filter.
        let mut games: Vec<(String, String, usize)> = Vec::new();
        for clip in &self.clips.entries {
            let key = clips::game_key(&clip.game);
            match games.iter_mut().find(|(k, ..)| *k == key) {
                Some(game) => {
                    game.2 += 1;
                    // Your folder's spelling of a game wins.
                    if clip.game_from_folder {
                        game.1 = clip.game.clone();
                    }
                }
                None => games.push((key, clip.game.clone(), 1)),
            }
        }
        games.sort_by_key(|(_, name, _)| name.to_lowercase());
        if self.game_filter.as_ref().is_some_and(|key| !games.iter().any(|(k, ..)| k == key)) {
            self.game_filter = None;
        }
        ui.horizontal(|ui| {
            ui.label("Sort");
            let sort_label = |sort: ClipSort| match sort {
                ClipSort::NewestFirst => "Newest first",
                ClipSort::OldestFirst => "Oldest first",
                ClipSort::ByGame => "By game",
            };
            let mut sort = self.config.clip_sort;
            egui::ComboBox::from_id_salt("clip-sort").width(120.0).selected_text(sort_label(sort)).show_ui(ui, |ui| {
                for option in [ClipSort::NewestFirst, ClipSort::OldestFirst, ClipSort::ByGame] {
                    ui.selectable_value(&mut sort, option, sort_label(option));
                }
            });
            if sort != self.config.clip_sort {
                self.config.clip_sort = sort;
                self.persist();
            }
            ui.add_space(8.0);
            ui.label("Game");
            let selected = match &self.game_filter {
                Some(key) => games.iter().find(|(k, ..)| k == key).map_or("All games".to_string(), |(_, name, _)| name.clone()),
                None => "All games".to_string(),
            };
            egui::ComboBox::from_id_salt("clip-game").width(220.0).selected_text(selected).height(420.0).show_ui(ui, |ui| {
                if ui.selectable_label(self.game_filter.is_none(), format!("All games ({})", self.clips.entries.len())).clicked() {
                    self.game_filter = None;
                }
                for (key, name, count) in &games {
                    if ui.selectable_label(self.game_filter.as_ref() == Some(key), format!("{name} ({count})")).clicked() {
                        self.game_filter = Some(key.clone());
                    }
                }
            });
        });

        let mut shown: Vec<&ClipEntry> = self
            .clips
            .entries
            .iter()
            .filter(|clip| self.game_filter.as_ref().is_none_or(|key| clips::game_key(&clip.game) == *key))
            .collect();
        match self.config.clip_sort {
            ClipSort::NewestFirst => shown.sort_by_key(|clip| std::cmp::Reverse(clip.recorded)),
            ClipSort::OldestFirst => shown.sort_by_key(|clip| clip.recorded),
            ClipSort::ByGame => shown.sort_by_key(|clip| (clips::game_key(&clip.game), std::cmp::Reverse(clip.recorded))),
        }

        let ctx = ui.ctx().clone();
        let mut opened = None;
        let by_game = self.config.clip_sort == ClipSort::ByGame;
        let thumbnails = &mut self.thumbnails;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // One run of cards per game when grouping, else one run of all.
            let mut rest = &shown[..];
            while let Some(first) = rest.first() {
                let run = if by_game {
                    let key = clips::game_key(&first.game);
                    rest.iter().take_while(|clip| clips::game_key(&clip.game) == key).count()
                } else {
                    rest.len()
                };
                let (group, after) = rest.split_at(run);
                rest = after;
                if by_game {
                    let key = clips::game_key(&first.game);
                    let title = games.iter().find(|(k, ..)| *k == key).map_or(first.game.as_str(), |(_, name, _)| name.as_str());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(title).size(16.0).strong());
                        ui.label(egui::RichText::new(format!("{} clip{}", group.len(), if group.len() == 1 { "" } else { "s" })).color(theme::TEXT_MUTED));
                    });
                }
                // Top-aligned so cards line up even while thumbnails are still loading.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true), |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(16.0, 16.0);
                    for clip in group {
                        // The same spelling of the game as the filter shows.
                        let key = clips::game_key(&clip.game);
                        let game = games.iter().find(|(k, ..)| *k == key).map_or(clip.game.as_str(), |(_, name, _)| name.as_str());
                        if clip_card(ui, clip, thumbnails, (!by_game).then_some(game)) {
                            opened = Some(ClipEditor::open(&ctx, clip));
                        }
                    }
                });
            }
        });
        if opened.is_some() {
            self.editor = opened;
        }
    }
}

/// One clip's card. Cards off screen only take their space: their
/// thumbnail and length are loaded once they're scrolled into view.
/// Returns whether the clip was clicked to open.
fn clip_card(ui: &mut egui::Ui, clip: &ClipEntry, thumbnails: &mut Thumbnails, game: Option<&str>) -> bool {
    let size = egui::vec2(CARD_THUMBNAIL.x, CARD_THUMBNAIL.y + 62.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return false;
    }
    let (texture, duration) = thumbnails.get(clip);
    let mut card = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    card.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
    let thumbnail = match texture {
        Some(texture) => card.add(
            egui::Image::new((texture.id(), CARD_THUMBNAIL)).corner_radius(6.0).sense(egui::Sense::click()),
        ),
        None => card.add_sized(CARD_THUMBNAIL, egui::Button::new("▶").corner_radius(6.0)),
    };
    let opened = thumbnail.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Open").clicked();
    card.add(egui::Label::new(clips::display_name(&clip.name)).truncate());
    card.horizontal(|ui| {
        let mut details = vec![duration.map(clips::format_duration).unwrap_or_else(|| "…".into()), clips::format_size(clip.size)];
        if let Some(game) = game {
            details.insert(0, game.to_string());
        }
        ui.add(egui::Label::new(egui::RichText::new(details.join(" · ")).small().color(theme::TEXT_MUTED)).truncate());
    });
    if card.small_button("Show in folder").clicked() {
        clips::show_in_folder(&clip.path);
    }
    opened
}

/// A folder name for a game's clips from its window title: characters
/// Windows doesn't allow in file names are dropped, and it's kept short.
fn folder_name(title: &str) -> Option<String> {
    let cleaned: String = title
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control())
        .take(60)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_folder_names_are_safe_and_short() {
        assert_eq!(folder_name("Deadlock").as_deref(), Some("Deadlock"));
        assert_eq!(folder_name("Half-Life 2: Episode Two").as_deref(), Some("Half-Life 2 Episode Two"));
        assert_eq!(folder_name("  ...  "), None);
        assert_eq!(folder_name(&"x".repeat(200)).map(|n| n.len()), Some(60));
    }
}
