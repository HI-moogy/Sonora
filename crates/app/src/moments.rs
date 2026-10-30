use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio_engine::{BusTap, Mixer};
use capture_engine::{
    default_clips_dir, AudioInput, CaptureTarget, ClipSaver, Recorder, Resolution, VideoSettings, DESKTOP_TRACK,
};
use common::{ClipSort, ClipTags, MomentsConfig, RecordMode};
use eframe::egui;

use crate::clips::editor::{ClipEditor, EditorAction, Neighbours};
use crate::clips::share::ShareWindow;
use crate::clips::tags::{self, TagCounts, TagUi};
use crate::clips::{self, ClipEntry, ClipList, Thumbnails};
use crate::game_library::{self, Library};
use crate::games::{self, AppWindow, GameFinder};
use crate::hotkey::{self, GlobalHotkey};
use crate::{app_label, capitalized, file_dialogs, sounds, theme, FIXED_BUSES};

/// How often the clips folder (and its subfolders) is looked through again.
const CLIP_SCAN_INTERVAL: Duration = Duration::from_secs(10);
/// How often to look for games (and which one is in front) to record.
const GAME_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// On-screen size of a clip's thumbnail in the Clips tab.
const CARD_THUMBNAIL: egui::Vec2 = egui::vec2(256.0, 144.0);

/// What clips can record, as the settings list them: the mode, its name,
/// and what it means.
const RECORD_MODES: [(RecordMode, &str, &str); 3] = [
    (
        RecordMode::ScreenWhilePlaying,
        "Whole screen, while a game is open",
        "The monitor the game is on, with anything else on it. Nothing is recorded while no game is open.",
    ),
    (
        RecordMode::Games,
        "Just the game's window",
        "Only the game, even when something covers it. Nothing is recorded while no game is open.",
    ),
    (RecordMode::Screen, "Whole screen, all the time", "Your main monitor, game or not."),
];

/// Shown with "Whole screen, all the time". Measured on a Radeon RX 6700
/// XT at 1080p 60 fps: its video encoder about 52% busy, nonstop.
const SCREEN_WARNING: &str = "⚠ Heavy on your graphics card: its video encoder works nonstop, even on an idle \
    desktop, and Task Manager can show Sonora using half the GPU. Games lose little, since the encoder is separate \
    from what draws them, but it uses power. The other two choices record only while a game is open.";

/// One row of the games list in clip settings.
struct GameEntry {
    name: String,
    /// Where it comes from: "Steam", "Added by you"...
    source: String,
    /// Searched besides the name: the program or folder.
    path: String,
    left_out: bool,
    open: bool,
    recording: bool,
    /// What its button does.
    action: GameAction,
}

/// A change to which games and apps start a recording.
#[derive(Clone)]
enum GameAction {
    /// Adds a choice (see [`games::covers`]) to `never_record`.
    LeaveOut(String),
    /// Takes out of `never_record` the game's choice, and any choice
    /// covering its open programs (`paths`).
    PutBack { choice: String, paths: Vec<String> },
    /// Takes a choice out of `always_record`.
    Remove(String),
    /// Records a program (a lowercase full path), wherever it's installed.
    Add(String),
}

/// Every audio track a clip has, in file order: one per bus, then desktop
/// audio (the default track).
fn clip_tracks() -> impl Iterator<Item = &'static str> {
    FIXED_BUSES.iter().copied().chain([DESKTOP_TRACK])
}

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
    /// Your clip tags, and which clips have them.
    tags: ClipTags,
    tag_ui: TagUi,
    /// Why your tags couldn't be saved.
    tags_error: Option<String>,
    /// The Share to Discord window, while it's open.
    sharing: Option<ShareWindow>,
    ctx: egui::Context,
    games: GameFinder,
    /// Running apps' windows as of the last check.
    apps: Vec<AppWindow>,
    /// What the recorder was last told to capture.
    target: CaptureTarget,
    /// The game's (or chosen app's) window that started the recording.
    game_window: Option<isize>,
    last_game_check: Option<Instant>,
    /// The installed games, for the games list and finding games.
    library: Library,
    library_scan: Option<Receiver<Library>>,
    /// What's typed in the games list's search box.
    game_search: String,
    /// Why the last program picked to add wasn't added.
    games_note: Option<String>,
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
            tags: common::load_clip_tags(),
            tag_ui: TagUi::default(),
            tags_error: None,
            sharing: None,
            ctx: ctx.clone(),
            games: GameFinder::new(),
            apps: Vec::new(),
            target: CaptureTarget::Screen,
            game_window: None,
            last_game_check: None,
            library: Library::default(),
            library_scan: Some(game_library::scan_in_background()),
            game_search: String::new(),
            games_note: None,
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
                self.game_window = None;
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
        if let Some(library) = self.library_scan.as_ref().and_then(|scan| scan.try_recv().ok()) {
            self.games.set_library(&library);
            self.library = library;
            self.library_scan = None;
            self.last_game_check = None;
        }
        if self.last_game_check.is_some_and(|at| at.elapsed() < GAME_CHECK_INTERVAL) {
            return;
        }
        self.last_game_check = Some(Instant::now());
        let mode = self.record_mode();
        let game = if mode == RecordMode::Screen {
            None
        } else {
            self.apps = self.games.app_windows();
            games::pick_window(&self.apps, &self.config.always_record, &self.config.never_record, self.game_window)
                .map(|app| app.window)
        };
        let target = match (mode, game) {
            (RecordMode::Screen, _) => CaptureTarget::Screen,
            (_, None) => CaptureTarget::Nothing,
            (RecordMode::ScreenWhilePlaying, Some(window)) => CaptureTarget::Monitor(games::monitor_of(window)),
            (RecordMode::Games, Some(window)) => CaptureTarget::Window(window),
        };
        if target != self.target {
            if let Some(recorder) = &self.recorder {
                recorder.set_target(target);
            }
            self.target = target;
        }
        if game != self.game_window {
            self.game_window = game;
            *self.shared.game.lock().unwrap() = self.recording_app().and_then(|app| folder_name(&app.title));
        }
    }

    fn record_mode(&self) -> RecordMode {
        self.config.record.unwrap_or(RecordMode::UNTIL_PICKED)
    }

    /// The game (or chosen app) that started the recording, if any.
    fn recording_app(&self) -> Option<&AppWindow> {
        let window = self.game_window?;
        self.apps.iter().find(|app| app.window == window)
    }

    /// Opens clip settings, and looks for installed games again for its
    /// games list (you may have installed one since).
    fn open_settings(&mut self) {
        self.show_settings = true;
        if self.library_scan.is_none() {
            self.library_scan = Some(game_library::scan_in_background());
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
        let what = match (self.record_mode(), self.recording_app()) {
            (RecordMode::Screen, _) => String::new(),
            (RecordMode::ScreenWhilePlaying, Some(app)) => format!("{} is open, recording its screen · ", app.title),
            (RecordMode::Games, Some(app)) => format!("Recording {} · ", app.title),
            (_, None) => "No game open, nothing being recorded · ".to_string(),
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
    /// The save-a-clip hotkey, as you'd press it ("Alt+F10").
    pub fn hotkey_label(&self) -> String {
        hotkey::label(self.config.hotkey).to_string()
    }

    pub fn tab(&mut self, ui: &mut egui::Ui, mixer: &Mixer) {
        if self.show_settings {
            egui::Panel::right("clip-settings")
                .size_range(380.0..=480.0)
                .default_size(430.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::surface())
                        .inner_margin(egui::Margin::same(16))
                        .stroke(egui::Stroke::new(1.0, theme::border())),
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
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::same(20)))
            .show(ui, |ui| match &mut self.editor {
            Some(editor) => {
                // The previous and next clips, as the clip list shows them.
                let filter = ClipFilter { game: self.game_filter.as_deref(), tag: self.tag_ui.filter.as_deref(), tags: &self.tags };
                let shown = shown_clips(&self.clips.entries, &filter, self.config.clip_sort);
                let source = editor.path().to_path_buf();
                let at = shown.iter().position(|clip| clip.path == editor.path());
                let neighbour = |step: isize| at.and_then(|at| at.checked_add_signed(step)).and_then(|i| shown.get(i).copied());
                let neighbours = Neighbours { previous: neighbour(-1).is_some(), next: neighbour(1).is_some() };
                let go = match editor.ui(ui, neighbours) {
                    EditorAction::None => None,
                    EditorAction::Close => {
                        self.editor = None;
                        None
                    }
                    EditorAction::Saved(saved) => {
                        // An edited copy keeps the clip's tags.
                        if saved != source {
                            let names: Vec<String> = self.tags.of(&source).into_iter().map(|tag| tag.name.clone()).collect();
                            for name in names {
                                self.tags.set(&saved, &name, true);
                            }
                            self.tags_error = save_tags(&self.tags);
                        }
                        self.last_scan = None;
                        None
                    }
                    EditorAction::Previous => neighbour(-1),
                    EditorAction::Next => neighbour(1),
                };
                if let Some(clip) = go {
                    self.editor = Some(ClipEditor::open(ui.ctx(), clip));
                }
            }
            None => self.clips_ui(ui),
        });
        let clips_dir = self.clips_dir();
        if let Some(window) = &mut self.sharing {
            let mut limit = self.config.share_limit_mb;
            let (open, settled) = window.show(ui.ctx(), &mut limit, &clips_dir);
            self.config.share_limit_mb = limit;
            if settled {
                self.persist();
            }
            if !open {
                self.sharing = None;
            }
        }
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
            ui.label(egui::RichText::new(text).color(if is_error { theme::danger() } else { theme::text_muted() }));
        });

        theme::section(ui, "What to record");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mode = self.record_mode();
            for (choice, label, meaning) in RECORD_MODES {
                if ui.radio(mode == choice, label).clicked() {
                    self.set_record_mode(choice);
                }
                ui.indent(label, |ui| ui.label(egui::RichText::new(meaning).small().color(theme::text_muted())));
            }
            if mode == RecordMode::Screen {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(SCREEN_WARNING).color(theme::warning()));
            }
        });
        if self.record_mode() != RecordMode::Screen {
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
                    .color(theme::text_muted()),
            );
        });

        theme::section(ui, "Hotkey");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.capturing_hotkey {
                ui.label("Press a key, or a combination… (Esc to cancel)");
                ui.label(
                    egui::RichText::new("Letters and numbers need Ctrl or Alt, so typing doesn't save clips.")
                        .small()
                        .color(theme::text_muted()),
                );
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
                if hotkey::passes_through(self.config.hotkey) {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} still does its usual thing in other apps too.",
                            hotkey::label(self.config.hotkey)
                        ))
                        .small()
                        .color(theme::text_muted()),
                    );
                }
            }
            if let Some(error) = &self.hotkey_error {
                ui.colored_label(theme::danger(), error);
            }
        });

        theme::section(ui, "Tags");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("Right-click a clip to tag it or make a tag, and pick a tag above the clips to see only those.")
                    .small()
                    .color(theme::text_muted()),
            );
            let counts = tags::counts(&self.tags, self.clips.entries.iter().map(|clip| clip.path.as_path()));
            if self.tag_ui.settings(ui, &mut self.tags, &counts) {
                self.tags_error = save_tags(&self.tags);
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
                .color(theme::text_muted()),
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

    /// The games and apps that start a recording: the installed games
    /// Sonora found, the apps you added, and games found while open. Any
    /// can be left out (and put back), and any program added.
    fn games_ui(&mut self, ui: &mut egui::Ui) {
        ui.weak(
            "Games from Steam, Epic and other launchers, and the ones Windows knows, are found on their own. \
             If several are open, the one in front is recorded.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.game_search)
                .hint_text("🔍 Search games and apps")
                .desired_width(f32::INFINITY),
        );
        let search = self.game_search.trim().to_lowercase();
        let matches = |name: &str, path: &str| {
            search.is_empty() || name.to_lowercase().contains(&search) || path.contains(&search)
        };
        let entries: Vec<GameEntry> =
            self.game_entries().into_iter().filter(|entry| matches(&entry.name, &entry.path)).collect();
        let (counted, left_out): (Vec<&GameEntry>, Vec<&GameEntry>) = entries.iter().partition(|entry| !entry.left_out);
        let mut action = None;

        egui::ScrollArea::vertical().id_salt("games-list").max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
            if counted.is_empty() {
                ui.weak(match (search.is_empty(), self.library_scan.is_some()) {
                    (false, _) => "Nothing matches.",
                    (true, true) => "Looking for installed games…",
                    (true, false) => "No games found. Add one below.",
                });
            }
            for entry in &counted {
                action = game_row(ui, entry, "Remove").or(action.take());
            }
        });
        if !left_out.is_empty() {
            egui::CollapsingHeader::new(format!("Left out ({})", left_out.len())).id_salt("games-left-out").show(
                ui,
                |ui| {
                    for entry in &left_out {
                        action = game_row(ui, entry, "Put back").or(action.take());
                    }
                },
            );
        }

        ui.add_space(8.0);
        ui.strong("Add a game or app");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Browse…").on_hover_text("Pick the program (.exe)").clicked() {
                if let Some(path) = file_dialogs::pick_program() {
                    action = Some(GameAction::Add(path.to_string_lossy().to_lowercase()));
                }
            }
            ui.weak("or one that's open:");
        });
        let mut seen = HashSet::new();
        for app in &self.apps {
            let counted = app.is_game || self.config.always_record.iter().any(|choice| games::covers(choice, &app.path));
            if counted || !seen.insert(app.exe.as_str()) || !matches(&app.title, &app.path) {
                continue;
            }
            ui.push_id(&app.path, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Add").clicked() {
                            action = Some(GameAction::Add(app.path.clone()));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(&app.title).truncate()).on_hover_text(&app.path);
                        });
                    });
                });
            });
        }
        if let Some(note) = &self.games_note {
            ui.label(egui::RichText::new(note).small().color(theme::text_muted()));
        }
        if let Some(action) = action {
            self.apply_game_action(action);
        }
    }

    /// The games list's rows: open ones first, then by name.
    fn game_entries(&self) -> Vec<GameEntry> {
        let (always, never) = (&self.config.always_record, &self.config.never_record);
        let recording = self.recording_app().map(|app| app.path.as_str());
        let open_paths = |choice: &str| -> Vec<String> {
            self.apps.iter().filter(|app| games::covers(choice, &app.path)).map(|app| app.path.clone()).collect()
        };
        let mut entries = Vec::new();
        for game in &self.library.games {
            let choice = game.choice();
            let paths = open_paths(&game.path);
            let left_out = never.iter().any(|c| *c == choice || paths.iter().any(|path| games::covers(c, path)));
            entries.push(GameEntry {
                name: game.name.clone(),
                source: game.source.to_string(),
                path: game.path.clone(),
                left_out,
                open: !paths.is_empty(),
                recording: recording.is_some_and(|path| games::covers(&game.path, path)),
                action: if left_out { GameAction::PutBack { choice, paths } } else { GameAction::LeaveOut(choice) },
            });
        }
        for choice in always {
            let open = self.apps.iter().find(|app| games::covers(choice, &app.path));
            let name = match open {
                Some(app) => app.title.clone(),
                None => capitalized(app_label(choice.trim_end_matches('\\').rsplit('\\').next().unwrap_or(choice))),
            };
            entries.push(GameEntry {
                name,
                source: "Added by you".into(),
                path: choice.clone(),
                left_out: false,
                open: open.is_some(),
                recording: recording.is_some_and(|path| games::covers(choice, path)),
                action: GameAction::Remove(choice.clone()),
            });
        }
        // Games open now that no scan listed: found by their folder, or on
        // Windows' list after the last scan.
        let mut seen = HashSet::new();
        for app in &self.apps {
            let listed = self.library.games.iter().any(|game| games::covers(&game.path, &app.path))
                || always.iter().any(|choice| games::covers(choice, &app.path));
            if !app.is_game || listed || !seen.insert(app.exe.as_str()) {
                continue;
            }
            let left_out = never.iter().any(|choice| games::covers(choice, &app.path));
            entries.push(GameEntry {
                name: app.title.clone(),
                source: "Found while open".into(),
                path: app.path.clone(),
                left_out,
                open: true,
                recording: recording == Some(app.path.as_str()),
                action: if left_out {
                    GameAction::PutBack { choice: app.exe.clone(), paths: vec![app.path.clone()] }
                } else {
                    GameAction::LeaveOut(app.exe.clone())
                },
            });
        }
        entries.sort_by_cached_key(|entry| (!entry.recording, !entry.open, entry.name.to_lowercase()));
        entries
    }

    fn apply_game_action(&mut self, action: GameAction) {
        self.games_note = None;
        let (always, never) = (&mut self.config.always_record, &mut self.config.never_record);
        match action {
            GameAction::LeaveOut(choice) => never.push(choice),
            GameAction::PutBack { choice, paths } => {
                never.retain(|c| *c != choice && !paths.iter().any(|path| games::covers(c, path)));
            }
            GameAction::Remove(choice) => always.retain(|c| *c != choice),
            GameAction::Add(path) => {
                let name = capitalized(app_label(games::exe_name(&path)));
                if never.iter().any(|choice| games::covers(choice, &path)) {
                    // A game you left out: put it back.
                    never.retain(|choice| !games::covers(choice, &path));
                } else if always.iter().any(|choice| games::covers(choice, &path)) {
                    self.games_note = Some(format!("{name} is already in the list."));
                } else if self.games.is_game(&path) {
                    self.games_note = Some(format!("{name} is a game Sonora already finds."));
                } else {
                    always.push(games::exe_name(&path).to_string());
                }
            }
        }
        self.persist();
        self.last_game_check = None;
        self.tick();
    }

    /// Asks once what clips should record.
    fn record_mode_question(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("What should clips record?");
            for (choice, label, meaning) in RECORD_MODES {
                ui.horizontal_wrapped(|ui| {
                    if ui.button(label).clicked() {
                        self.set_record_mode(choice);
                        if choice != RecordMode::Screen {
                            self.open_settings();
                        }
                    }
                    ui.weak(meaning);
                });
            }
            ui.label(egui::RichText::new(SCREEN_WARNING).small().color(theme::warning()));
            ui.weak("Until you pick, clips record the whole screen while a game is open. You can change this later in ⚙ settings.");
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
                    if self.show_settings {
                        self.show_settings = false;
                    } else {
                        self.open_settings();
                    }
                }
                crate::guide::target(ui.ctx(), "clip-settings", gear.rect);
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
            ui.colored_label(theme::danger(), status);
        } else if self.record_mode() != RecordMode::Screen && self.recorder.is_some() {
            ui.weak(match self.recording_app() {
                Some(app) => format!(
                    "Recording {}{} · press {} anywhere to save the last {}",
                    app.title,
                    if self.record_mode() == RecordMode::ScreenWhilePlaying { "'s screen" } else { "" },
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
            ui.colored_label(theme::danger(), format!("Couldn't save the last clip: {error}"));
        }
        ui.horizontal(|ui| {
            ui.label("Saved to");
            ui.strong(dir.display().to_string());
            if ui.button("Open folder").clicked() {
                crate::support::open_folder(&dir);
            }
            if ui.button("Change folder…").clicked() {
                if let Some(picked) = crate::file_dialogs::pick_folder(&dir) {
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
            .color(theme::text_muted()));
            return;
        }

        // Each game with how many clips it has, for the filter.
        let counts = tags::counts(&self.tags, self.clips.entries.iter().map(|clip| clip.path.as_path()));
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
            if !self.tags.tags.is_empty() {
                ui.add_space(8.0);
                ui.label("Tag");
                self.tag_ui.filter(ui, &self.tags, &counts);
            }
        });
        if let Some(error) = &self.tags_error {
            ui.colored_label(theme::danger(), error);
        }

        let filter = ClipFilter { game: self.game_filter.as_deref(), tag: self.tag_ui.filter.as_deref(), tags: &self.tags };
        let shown = shown_clips(&self.clips.entries, &filter, self.config.clip_sort);
        if shown.is_empty() {
            if let Some(tag) = &self.tag_ui.filter {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("No clips tagged {tag} yet. Right-click a clip to tag it."))
                        .color(theme::text_muted()),
                );
            }
        }

        let ctx = ui.ctx().clone();
        let mut opened = None;
        let mut tagged = false;
        let mut sharing = None;
        let by_game = self.config.clip_sort == ClipSort::ByGame;
        let thumbnails = &mut self.thumbnails;
        let (tags, tag_ui) = (&mut self.tags, &mut self.tag_ui);
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
                        ui.label(egui::RichText::new(format!("{} clip{}", group.len(), if group.len() == 1 { "" } else { "s" })).color(theme::text_muted()));
                    });
                }
                // Top-aligned so cards line up even while thumbnails are still loading.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true), |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(16.0, 16.0);
                    for clip in group {
                        // The same spelling of the game as the filter shows.
                        let key = clips::game_key(&clip.game);
                        let game = games.iter().find(|(k, ..)| *k == key).map_or(clip.game.as_str(), |(_, name, _)| name.as_str());
                        let card = clip_card(ui, clip, thumbnails, (!by_game).then_some(game), tags, tag_ui, &counts);
                        if card.opened {
                            opened = Some(ClipEditor::open(&ctx, clip));
                        }
                        if card.share {
                            sharing = Some(ShareWindow::open(&clip.path, &clip.name));
                        }
                        tagged |= card.tagged;
                    }
                });
            }
        });
        if opened.is_some() {
            self.editor = opened;
        }
        if tagged {
            self.tags_error = save_tags(&self.tags);
        }
        if sharing.is_some() {
            self.sharing = sharing;
        }
    }
}

fn save_tags(tags: &ClipTags) -> Option<String> {
    common::save_clip_tags(tags).err().map(|error| format!("Couldn't save your tags: {error}"))
}

/// Which clips the clip list shows.
struct ClipFilter<'a> {
    /// Only this game's (a [`clips::game_key`]).
    game: Option<&'a str>,
    /// Only those with this tag.
    tag: Option<&'a str>,
    tags: &'a ClipTags,
}

/// The clips the clip list shows, in its order: those `filter` lets
/// through, sorted by `sort`. The editor's previous and next clip follow
/// this too.
fn shown_clips<'a>(entries: &'a [ClipEntry], filter: &ClipFilter, sort: ClipSort) -> Vec<&'a ClipEntry> {
    let mut shown: Vec<&ClipEntry> = entries
        .iter()
        .filter(|clip| filter.game.is_none_or(|key| clips::game_key(&clip.game) == key))
        .filter(|clip| filter.tag.is_none_or(|tag| filter.tags.has(&clip.path, tag)))
        .collect();
    match sort {
        ClipSort::NewestFirst => shown.sort_by_key(|clip| std::cmp::Reverse(clip.recorded)),
        ClipSort::OldestFirst => shown.sort_by_key(|clip| clip.recorded),
        ClipSort::ByGame => shown.sort_by_key(|clip| (clips::game_key(&clip.game), std::cmp::Reverse(clip.recorded))),
    }
    shown
}

/// What happened on a clip's card.
#[derive(Default)]
struct CardResponse {
    /// It was clicked to open.
    opened: bool,
    /// Its tags changed (from its right-click menu).
    tagged: bool,
    /// Share to Discord was picked from its right-click menu.
    share: bool,
}

/// One clip's card, with its tags on its thumbnail; right-click it to tag
/// it. Cards off screen only take their space: their thumbnail and length
/// are loaded once they're scrolled into view.
fn clip_card(
    ui: &mut egui::Ui,
    clip: &ClipEntry,
    thumbnails: &mut Thumbnails,
    game: Option<&str>,
    tags: &mut ClipTags,
    tag_ui: &mut TagUi,
    counts: &TagCounts,
) -> CardResponse {
    let size = egui::vec2(CARD_THUMBNAIL.x, CARD_THUMBNAIL.y + 62.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return CardResponse::default();
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
    let thumbnail = thumbnail.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Open · right-click to tag");
    let opened = thumbnail.clicked();
    tags::chips(&card, thumbnail.rect, &tags.of(&clip.path));
    card.add(egui::Label::new(clips::display_name(&clip.name)).truncate());
    card.horizontal(|ui| {
        let mut details = vec![duration.map(clips::format_duration).unwrap_or_else(|| "…".into()), clips::format_size(clip.size)];
        if let Some(game) = game {
            details.insert(0, game.to_string());
        }
        ui.add(egui::Label::new(egui::RichText::new(details.join(" · ")).small().color(theme::text_muted())).truncate());
    });
    if card.small_button("Show in folder").clicked() {
        clips::show_in_folder(&clip.path);
    }
    // Right-clicking anywhere on the card, the thumbnail too. It stays open
    // while you tick tags.
    let response = card.response();
    let open = response.container_secondary_clicked().then_some(egui::SetOpenCommand::Bool(true));
    let mut share = false;
    let tagged = egui::Popup::menu(&response)
        .open_memory(open)
        .at_pointer_fixed()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let changed = tag_ui.menu(ui, &clip.path, tags, counts);
            ui.separator();
            if ui.button("Share to Discord…").clicked() {
                share = true;
                ui.close();
            }
            if ui.button("Show in folder").clicked() {
                clips::show_in_folder(&clip.path);
                ui.close();
            }
            changed
        })
        .is_some_and(|menu| menu.inner);
    CardResponse { opened, tagged, share }
}

/// One row of the games list: the name, where it's from and whether it's
/// open, and a button (`button`) that does the row's action.
fn game_row(ui: &mut egui::Ui, entry: &GameEntry, button: &str) -> Option<GameAction> {
    let mut clicked = false;
    // Rows move as games open and close, and egui matches a click to the
    // previous frame's layout: ids that follow the game, not the row, keep
    // a click on the game you aimed at.
    ui.push_id((&entry.source, &entry.path), |ui| {
        ui.horizontal(|ui| {
            // The button keeps its place on the right; the name gets the
            // rest and is cut short if needed.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = ui.small_button(button).clicked();
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.add(egui::Label::new(&entry.name).truncate()).on_hover_text(&entry.path);
                    let (note, color) = match (entry.recording, entry.open) {
                        (true, _) => (format!("{} · recording now", entry.source), theme::accent()),
                        (false, true) => (format!("{} · open", entry.source), theme::text_muted()),
                        (false, false) => (entry.source.clone(), theme::text_muted()),
                    };
                    ui.label(egui::RichText::new(note).small().color(color));
                });
            });
        });
    });
    ui.separator();
    clicked.then(|| entry.action.clone())
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
