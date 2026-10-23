//! The clip editor, after SteelSeries Moments' clip viewer: plays a saved
//! clip with a waveform and volume slider for each audio track, and saves
//! the clip with that mix as its default audio.
//!
//! A clip's audio tracks are the buses (only apps routed to them in the
//! Mixer), then Desktop (everything heard, routed apps included), then, once
//! edited, the mix this editor wrote. Desktop can't be turned down per app,
//! so the mix is built from the buses and Desktop starts out of it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio_engine::atomic_f32::AtomicF32;
use audio_engine::ComGuard;
use capture_engine::{decode_clip_audio, save_edited_clip, ClipAudio, ClipEdit, PreviewFrame, TrackAudio, VideoPreview};
use eframe::egui;

use super::{format_duration, format_size, play, show_in_folder, ClipEntry};
use crate::icons::{big_icon_button, icon_button, Icon};
use crate::{theme, FIXED_BUSES};

const SAMPLE_RATE: u64 = 48_000;
const HNS_PER_SECOND: i64 = 10_000_000;
/// Waveforms keep one peak per 10 ms.
const PEAK_FRAMES: usize = 480;
/// Waveforms show this many dB below full scale, so quiet tracks are visible.
const WAVEFORM_RANGE_DB: f32 = 48.0;
const PREVIEW_MAX_WIDTH: u32 = 960;
/// Audio queued ahead of what's heard. Small, so moving a slider is heard
/// right away.
const PLAYBACK_LEAD_FRAMES: u32 = SAMPLE_RATE as u32 * 60 / 1000;
/// Playback pauses when the editor hasn't been drawn for this long (another
/// tab is showing, or the window is minimized).
const UNSEEN_PAUSE: Duration = Duration::from_millis(400);
const SKIP: Duration = Duration::from_secs(5);
const MAX_VOLUME: f32 = 2.0;

const LABEL_WIDTH: f32 = 210.0;
/// Narrower than this, the save buttons go on a row below the transport so
/// the two don't overlap.
const ONE_ROW_CONTROLS_WIDTH: f32 = 900.0;
/// Width of the five play buttons and the gaps between them.
const TRANSPORT_WIDTH: f32 = 38.0 * 4.0 + 46.0 + 8.0 * 4.0;
/// The previous/next clip buttons either side of the video, and their gap
/// to it.
const SIDE_BUTTON: f32 = 56.0;
const SIDE_GAP: f32 = 16.0;
const RULER_HEIGHT: f32 = 22.0;
/// Tall enough for a track's name above its volume slider.
const ROW_HEIGHT: f32 = 60.0;
const ROW_GAP: f32 = 6.0;

/// What a clip's audio track holds, from its position (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Bus(&'static str),
    Desktop,
    /// The mix an earlier edit wrote; replaced by the new one.
    EarlierMix,
}

fn role(index: usize) -> Role {
    match FIXED_BUSES.get(index) {
        Some(&bus) => Role::Bus(bus),
        None if index == FIXED_BUSES.len() => Role::Desktop,
        None => Role::EarlierMix,
    }
}

pub enum EditorAction {
    None,
    Close,
    /// A clip was written; the clip list should pick it up.
    Saved,
    /// Open the clip before or after this one in the clip list.
    Previous,
    Next,
}

/// Whether the clip list has a clip before and after the open one.
#[derive(Clone, Copy, Default)]
pub struct Neighbours {
    pub previous: bool,
    pub next: bool,
}

struct Row {
    /// Index of the clip's audio track.
    track: usize,
    role: Role,
    volume: f32,
    /// Peak level per [`PEAK_FRAMES`]; empty if the track is silent.
    peaks: Vec<f32>,
}

impl Row {
    fn default_volume(&self) -> f32 {
        if self.role == Role::Desktop { 0.0 } else { 1.0 }
    }

    fn label(&self) -> &'static str {
        match self.role {
            Role::Bus(bus) => bus,
            Role::Desktop => "Desktop (everything)",
            Role::EarlierMix => "Earlier mix",
        }
    }

    fn color(&self) -> egui::Color32 {
        match self.role {
            Role::Bus(_) => theme::channel_color(self.track),
            _ => theme::desktop_track(),
        }
    }
}

struct Loaded {
    audio: Arc<ClipAudio>,
    peaks: Vec<Vec<f32>>,
}

enum Saving {
    Idle,
    Running { overwrite: bool, result: Receiver<Result<PathBuf, String>> },
    Done(Result<PathBuf, String>),
}

pub struct ClipEditor {
    path: PathBuf,
    name: String,
    size: u64,
    loading: Option<Receiver<Result<Loaded, String>>>,
    error: Option<String>,
    audio: Option<Arc<ClipAudio>>,
    rows: Vec<Row>,
    player: Option<Player>,
    /// `None` while an overwrite has the file.
    preview: Option<VideoPreview>,
    frame: Option<(i64, egui::TextureHandle)>,
    next_frame: Option<PreviewFrame>,
    /// Position while paused or before the audio is ready (100 ns units).
    paused_at: i64,
    last_drag_seek: Option<Instant>,
    saving: Saving,
}

impl ClipEditor {
    /// The open clip's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(ctx: &egui::Context, clip: &ClipEntry) -> Self {
        Self::open_path(ctx, &clip.path, clip.size)
    }

    fn open_path(ctx: &egui::Context, path: &Path, size: u64) -> Self {
        let (tx, loading) = mpsc::channel();
        let (load_path, repaint) = (path.to_path_buf(), ctx.clone());
        std::thread::Builder::new()
            .name("clip-load".into())
            .spawn(move || {
                let loaded = decode_clip_audio(&load_path).map_err(|e| e.to_string()).map(|audio| Loaded {
                    peaks: audio.tracks.iter().map(peaks).collect(),
                    audio: Arc::new(audio),
                });
                let _ = tx.send(loaded);
                repaint.request_repaint();
            })
            .expect("spawn clip load thread");
        Self {
            path: path.to_path_buf(),
            name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            size,
            loading: Some(loading),
            error: None,
            audio: None,
            rows: Vec::new(),
            player: None,
            preview: Some(open_preview(ctx, path)),
            frame: None,
            next_frame: None,
            paused_at: 0,
            last_drag_seek: None,
            saving: Saving::Idle,
        }
    }

    fn duration(&self) -> i64 {
        self.audio.as_ref().map_or(0, |audio| frames_to_hns(audio.frames as u64))
    }

    fn position(&self) -> i64 {
        match &self.player {
            Some(player) => frames_to_hns(player.position()),
            None => self.paused_at,
        }
    }

    fn is_playing(&self) -> bool {
        self.player.as_ref().is_some_and(Player::is_playing)
    }

    fn seek(&mut self, to: i64) {
        let to = to.clamp(0, self.duration().max(0));
        self.paused_at = to;
        if let Some(player) = &self.player {
            player.seek(hns_to_frames(to));
        }
        if let Some(preview) = &mut self.preview {
            preview.seek(to);
        }
        self.next_frame = None;
    }

    fn toggle_play(&mut self) {
        let Some(player) = &self.player else { return };
        if player.is_playing() {
            player.set_playing(false);
        } else {
            if self.position() >= self.duration() {
                self.seek(0);
            }
            if let Some(player) = &self.player {
                player.set_playing(true);
            }
        }
    }

    /// The mix as each track's weight, shared by preview and save.
    fn mix(&self) -> Vec<f32> {
        let count = self.audio.as_ref().map_or(0, |audio| audio.tracks.len());
        let mut mix = vec![0.0; count];
        for row in &self.rows {
            mix[row.track] = row.volume;
        }
        mix
    }

    /// Bus tracks are saved at their volume (as Moments' clip volumes would
    /// have recorded them); Desktop is kept as is, since it's the only copy
    /// of unrouted audio; the earlier mix is replaced.
    fn edit(&self) -> ClipEdit {
        let count = self.audio.as_ref().map_or(0, |audio| audio.tracks.len());
        let keep = (0..count)
            .map(|track| match role(track) {
                Role::Bus(_) => Some(self.rows.iter().find(|r| r.track == track).map_or(1.0, |r| r.volume)),
                Role::Desktop => Some(1.0),
                Role::EarlierMix => None,
            })
            .collect();
        ClipEdit { keep, mix: self.mix() }
    }

    fn is_edited(&self) -> bool {
        self.rows.iter().any(|row| row.volume != row.default_volume())
    }

    fn start_save(&mut self, ctx: &egui::Context, overwrite: bool) {
        let Some(audio) = self.audio.clone() else { return };
        if let Some(player) = &self.player {
            player.set_playing(false);
        }
        let destination = if overwrite { self.path.clone() } else { edited_path(&self.path) };
        if overwrite {
            // The preview holds the file open, which would block replacing it.
            self.preview = None;
        }
        let (source, edit) = (self.path.clone(), self.edit());
        let (tx, result) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("clip-edit-save".into())
            .spawn(move || {
                let saved = save_edited_clip(&source, &destination, &audio, &edit).map(|()| destination);
                let _ = tx.send(saved.map_err(|e| e.to_string()));
                repaint.request_repaint();
            })
            .expect("spawn clip save thread");
        self.saving = Saving::Running { overwrite, result };
    }

    /// Picks up background results: decoded audio, decoded frames, saves.
    fn poll(&mut self, ctx: &egui::Context) -> EditorAction {
        if let Some(loading) = &self.loading {
            // A loader that stopped without answering hit a damaged file.
            let loaded = match loading.try_recv() {
                Ok(loaded) => Some(loaded),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("the file looks damaged".to_string())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(loaded) = loaded {
                self.loading = None;
                match loaded {
                    Ok(loaded) => self.loaded(ctx, loaded),
                    Err(error) => self.error = Some(format!("Couldn't open this clip: {error}")),
                }
            }
        }

        let mut action = EditorAction::None;
        if let Saving::Running { overwrite, result } = &self.saving {
            let saved = match result.try_recv() {
                Ok(saved) => Some(saved),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("the clip couldn't be read".to_string())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(saved) = saved {
                let overwrite = *overwrite;
                if saved.is_ok() {
                    action = EditorAction::Saved;
                }
                if overwrite {
                    // Start over on the rewritten clip: its tracks now hold
                    // the saved volumes. On failure the original is intact.
                    let size = std::fs::metadata(&self.path).map_or(self.size, |m| m.len());
                    *self = Self::open_path(ctx, &self.path.clone(), size);
                }
                self.saving = Saving::Done(saved);
            }
        }

        if let Some(player) = &self.player {
            player.seen();
            if let Some(error) = player.error() {
                self.error = Some(format!("Can't play audio: {error}"));
            }
        }
        self.update_frame(ctx);
        action
    }

    fn loaded(&mut self, ctx: &egui::Context, loaded: Loaded) {
        self.rows = loaded
            .peaks
            .into_iter()
            .enumerate()
            .filter(|&(track, _)| role(track) != Role::EarlierMix)
            .map(|(track, peaks)| {
                let mut row = Row { track, role: role(track), volume: 1.0, peaks };
                row.volume = row.default_volume();
                row
            })
            .collect();
        let player = Player::start(ctx, loaded.audio.clone());
        self.audio = Some(loaded.audio);
        let mix = self.mix();
        player.set_gains(&mix);
        player.seek(hns_to_frames(self.paused_at));
        self.player = Some(player);
    }

    /// Shows the latest decoded frame due at the current position.
    fn update_frame(&mut self, ctx: &egui::Context) {
        let Some(preview) = &self.preview else { return };
        let position = self.position();
        let mut due = None;
        loop {
            if self.next_frame.is_none() {
                match preview.next_frame() {
                    Some(Ok(frame)) if frame.seek == preview.current_seek() => self.next_frame = Some(frame),
                    Some(Ok(_stale)) => continue,
                    Some(Err(error)) => {
                        self.error = Some(format!("Can't play video: {error}"));
                        break;
                    }
                    None => break,
                }
            }
            let next = self.next_frame.as_ref().expect("filled above");
            if self.frame.is_none() && due.is_none() || next.pts <= position {
                due = self.next_frame.take();
            } else {
                break;
            }
        }
        if let Some(frame) = due {
            let image = egui::ColorImage::from_rgba_premultiplied([frame.width, frame.height], &frame.rgba);
            match &mut self.frame {
                Some((pts, texture)) => {
                    texture.set(image, egui::TextureOptions::LINEAR);
                    *pts = frame.pts;
                }
                None => {
                    self.frame = Some((frame.pts, ctx.load_texture("clip-preview", image, egui::TextureOptions::LINEAR)))
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, neighbours: Neighbours) -> EditorAction {
        let ctx = ui.ctx().clone();
        let mut action = self.poll(&ctx);
        if self.is_playing() {
            ctx.request_repaint();
        }
        let typing = ctx.egui_wants_keyboard_input();
        if !typing && ui.input(|i| i.key_pressed(egui::Key::Space)) {
            self.toggle_play();
        }

        ui.horizontal(|ui| {
            if ui.button("← Clips").clicked() {
                action = EditorAction::Close;
            }
            ui.heading(crate::clips::display_name(&self.name));
            ui.weak(format!("{} · {}", format_duration(hns_to_duration(self.duration())), format_size(self.size)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Show in folder").clicked() {
                    show_in_folder(&self.path);
                }
                if ui.button("Open in player").on_hover_text("Plays the saved clip in your default video player").clicked() {
                    play(&self.path);
                }
            });
        });
        if let Some(error) = &self.error {
            ui.colored_label(theme::danger(), error);
        }
        ui.separator();

        let timeline_height = RULER_HEIGHT + self.rows.len().max(1) as f32 * (ROW_HEIGHT + ROW_GAP) + 8.0;
        let controls_height = if ui.available_width() >= ONE_ROW_CONTROLS_WIDTH { 44.0 } else { 80.0 };
        let video_height = (ui.available_height() - timeline_height - controls_height - 16.0).max(120.0);
        if let Some(go) = self.video_ui(ui, video_height, neighbours) {
            action = go;
        }
        ui.add_space(4.0);
        self.controls_ui(ui, &ctx);
        ui.add_space(4.0);
        self.timeline_ui(ui);
        action
    }

    /// The video, with the previous and next clip buttons either side;
    /// returns which of those was clicked.
    fn video_ui(&mut self, ui: &mut egui::Ui, height: f32, neighbours: Neighbours) -> Option<EditorAction> {
        let width = ui.available_width();
        let (full, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let area = full.shrink2(egui::vec2(SIDE_BUTTON + SIDE_GAP, 0.0));
        let size = match &self.frame {
            Some((_, texture)) => {
                let [w, h] = texture.size().map(|v| v as f32);
                let scale = (area.width() / w).min(area.height() / h);
                egui::vec2(w * scale, h * scale)
            }
            None => egui::vec2(area.height() * 16.0 / 9.0, area.height()).min(area.size()),
        };
        let rect = egui::Rect::from_center_size(area.center(), size);
        let painter = ui.painter_at(area);
        painter.rect_filled(rect, 4.0, egui::Color32::BLACK);
        match &self.frame {
            Some((_, texture)) => {
                egui::Image::new((texture.id(), size)).corner_radius(4.0).paint_at(ui, rect);
            }
            None => {
                let text = if self.preview.is_some() { "Loading…" } else { "Saving…" };
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(16.0), egui::Color32::GRAY);
            }
        }

        // Beside the video, centered on it.
        let mut go = None;
        let sides = [
            (rect.left() - SIDE_GAP - SIDE_BUTTON / 2.0, Icon::Previous, neighbours.previous, "Previous clip", EditorAction::Previous),
            (rect.right() + SIDE_GAP + SIDE_BUTTON / 2.0, Icon::Next, neighbours.next, "Next clip", EditorAction::Next),
        ];
        for (x, icon, enabled, hint, action) in sides {
            let button = egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(SIDE_BUTTON, SIDE_BUTTON));
            // In a child so the button doesn't move the layout's cursor; each
            // with its own id, or a click on one would count for both.
            let mut side = ui.new_child(egui::UiBuilder::new().id_salt(hint).max_rect(button));
            let response = big_icon_button(&mut side, icon, SIDE_BUTTON, enabled);
            if response.on_hover_text(if enabled { hint } else { "No more clips this way" }).clicked() {
                go = Some(action);
            }
        }
        go
    }

    /// The time on the left, play controls in the middle, saving on the
    /// right; on a narrow window, saving goes on a second line.
    fn controls_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let one_row = ui.available_width() >= ONE_ROW_CONTROLS_WIDTH;
        let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
        let position = self.position().min(self.duration());
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)));
        left.label(
            egui::RichText::new(format!("{} / {}", format_duration_precise(position), format_duration_precise(self.duration())))
                .monospace()
                .color(theme::text_muted()),
        );
        let transport = egui::Rect::from_center_size(row.center(), egui::vec2(TRANSPORT_WIDTH, row.height()));
        let mut middle = ui.new_child(egui::UiBuilder::new().max_rect(transport).layout(egui::Layout::left_to_right(egui::Align::Center)));
        self.transport_ui(&mut middle);
        if one_row {
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
            self.save_ui(&mut right, ctx);
        } else {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 28.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| self.save_ui(ui, ctx),
            );
        }
    }

    /// Back to the start, back 5 s, play/pause, forward 5 s, to the end.
    fn transport_ui(&mut self, ui: &mut egui::Ui) {
        let ready = self.player.is_some();
        let position = self.position().min(self.duration());
        ui.spacing_mut().item_spacing.x = 8.0;
        if icon_button(ui, Icon::ToStart, false, ready).on_hover_text("Back to the start").clicked() {
            self.seek(0);
        }
        if icon_button(ui, Icon::Back, false, ready).on_hover_text("Back 5 seconds").clicked() {
            self.seek(position - SKIP.as_nanos() as i64 / 100);
        }
        let icon = if self.is_playing() { Icon::Pause } else { Icon::Play };
        if icon_button(ui, icon, true, ready).on_hover_text("Play / pause (Space)").clicked() {
            self.toggle_play();
        }
        if icon_button(ui, Icon::Forward, false, ready).on_hover_text("Forward 5 seconds").clicked() {
            self.seek(position + SKIP.as_nanos() as i64 / 100);
        }
        if icon_button(ui, Icon::ToEnd, false, ready).on_hover_text("To the end").clicked() {
            if let Some(player) = &self.player {
                player.set_playing(false);
            }
            self.seek(self.duration());
        }
    }

    /// Reset, save buttons and save status, laid out right to left.
    fn save_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let ready = self.player.is_some();
        let busy = matches!(self.saving, Saving::Running { .. });
        let can_save = ready && !busy;
        let overwrite = ui
            .add_enabled(can_save && self.is_edited(), egui::Button::new("Overwrite"))
            .on_hover_text("Replace this clip with the new mix")
            .on_disabled_hover_text("Change a volume first");
        if overwrite.clicked() {
            self.start_save(ctx, true);
        }
        let save_new = ui
            .add_enabled(can_save && self.is_edited(), egui::Button::new(egui::RichText::new("Save as new clip").strong()))
            .on_hover_text("Save a copy with the new mix; this clip stays as it is")
            .on_disabled_hover_text("Change a volume first");
        if save_new.clicked() {
            self.start_save(ctx, false);
        }
        if ui.add_enabled(ready && self.is_edited() && !busy, egui::Button::new("Reset")).clicked() {
            for row in &mut self.rows {
                row.volume = row.default_volume();
            }
            if let Some(player) = &self.player {
                player.set_gains(&self.mix());
            }
        }
        match &self.saving {
            Saving::Idle => {}
            Saving::Running { .. } => {
                ui.spinner();
                ui.label("Saving…");
            }
            Saving::Done(Ok(path)) => {
                if path != &self.path {
                    if ui.small_button("Show").clicked() {
                        show_in_folder(path);
                    }
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(format!("Saved as {name}"));
                } else {
                    ui.label("Saved");
                }
            }
            Saving::Done(Err(error)) => {
                ui.colored_label(theme::danger(), format!("Couldn't save: {error}"));
            }
        }
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let height = RULER_HEIGHT + self.rows.len() as f32 * (ROW_HEIGHT + ROW_GAP);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height.max(RULER_HEIGHT + ROW_HEIGHT)), egui::Sense::hover());
        let lanes = egui::Rect::from_min_max(egui::pos2(rect.left() + LABEL_WIDTH + 8.0, rect.top()), rect.max);
        let duration = self.duration();
        let x_of = |t: i64| lanes.left() + lanes.width() * (t as f32 / duration.max(1) as f32);

        // Seek by clicking or dragging anywhere over the lanes.
        let scrub = ui
            .interact(lanes, ui.scope_id().with("clip-timeline"), egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if let Some(pointer) = scrub.interact_pointer_pos().filter(|_| self.player.is_some()) {
            let throttled = scrub.dragged() && self.last_drag_seek.is_some_and(|at| at.elapsed() < Duration::from_millis(60));
            if (scrub.clicked() || scrub.drag_started() || scrub.drag_stopped() || scrub.dragged()) && !throttled {
                let fraction = ((pointer.x - lanes.left()) / lanes.width()).clamp(0.0, 1.0);
                self.seek((duration as f64 * fraction as f64) as i64);
                self.last_drag_seek = Some(Instant::now());
            }
        }

        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        // Ruler: a labeled tick at a round interval that leaves room for labels.
        let ruler = egui::Rect::from_min_size(lanes.min, egui::vec2(lanes.width(), RULER_HEIGHT));
        let secs = duration as f64 / HNS_PER_SECOND as f64;
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .into_iter()
            .find(|&step| lanes.width() as f64 * step / secs.max(1e-3) >= 70.0)
            .unwrap_or(600.0);
        let mut t = 0.0;
        while t <= secs + 1e-6 && duration > 0 {
            let x = x_of((t * HNS_PER_SECOND as f64) as i64);
            painter.line_segment([egui::pos2(x, ruler.bottom() - 6.0), egui::pos2(x, ruler.bottom())], visuals.widgets.noninteractive.fg_stroke);
            let align = if x > ruler.right() - 30.0 { egui::Align2::RIGHT_TOP } else { egui::Align2::LEFT_TOP };
            painter.text(egui::pos2(x + 2.0, ruler.top() + 2.0), align, format_duration(Duration::from_secs_f64(t)), egui::FontId::proportional(11.0), visuals.weak_text_color());
            t += step;
        }

        let mut gains_changed = false;
        for (index, row) in self.rows.iter_mut().enumerate() {
            let top = rect.top() + RULER_HEIGHT + index as f32 * (ROW_HEIGHT + ROW_GAP);
            let label_rect = egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(LABEL_WIDTH, ROW_HEIGHT));
            let lane = egui::Rect::from_min_max(egui::pos2(lanes.left(), top), egui::pos2(lanes.right(), top + ROW_HEIGHT));
            let color = row.color();

            painter.rect_filled(label_rect, 6.0, theme::card_bg());
            painter.rect_filled(
                egui::Rect::from_min_size(label_rect.min, egui::vec2(3.0, label_rect.height())),
                egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 },
                color,
            );
            let mut label_ui = ui.new_child(egui::UiBuilder::new().max_rect(label_rect.shrink2(egui::vec2(10.0, 5.0))));
            label_ui.spacing_mut().item_spacing.y = 2.0;
            label_ui.spacing_mut().interact_size.y = 22.0;
            label_ui.horizontal(|ui| {
                ui.label(egui::RichText::new(row.label()).color(color).strong());
                if row.peaks.is_empty() {
                    ui.label(egui::RichText::new("silent").small().color(theme::text_muted()));
                }
            });
            // The slider plus its value box fill the header's width.
            label_ui.spacing_mut().slider_width = LABEL_WIDTH - 20.0 - 62.0;
            let slider = label_ui.add(
                egui::Slider::new(&mut row.volume, 0.0..=MAX_VOLUME).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
            let slider = if row.role == Role::Desktop {
                slider.on_hover_text(
                    "Everything you heard, including apps routed to a bus (so raising it brings those back too). \
                     It's the only track with apps you didn't route. Kept as is in the saved clip; this sets how much of it is in the mix.",
                )
            } else {
                slider.on_hover_text("This track's volume in the saved clip and in its mix")
            };
            gains_changed |= slider.changed();

            // Waveform as recorded, faint, with the level at this volume on
            // top, so a turned-down track still shows what it holds.
            painter.rect_filled(lane, 4.0, color.gamma_multiply(0.18));
            if row.peaks.is_empty() {
                painter.line_segment([egui::pos2(lane.left(), lane.center().y), egui::pos2(lane.right(), lane.center().y)], egui::Stroke::new(1.0, color.gamma_multiply(0.5)));
            } else {
                let (mut recorded, mut adjusted) = (egui::Mesh::default(), egui::Mesh::default());
                let columns = lane.width().max(1.0) as usize;
                let total = row.peaks.len();
                let half = (ROW_HEIGHT - 6.0) / 2.0;
                let bar = |x: f32, height: f32| {
                    egui::Rect::from_min_max(egui::pos2(x, lane.center().y - height), egui::pos2(x + 1.0, lane.center().y + height))
                };
                for column in 0..columns {
                    let from = (column * total / columns).min(total - 1);
                    let to = ((column + 1) * total / columns).clamp(from + 1, total);
                    let peak = row.peaks[from..to].iter().fold(0.0f32, |a, &b| a.max(b));
                    let x = lane.left() + column as f32;
                    let height = waveform_height(peak) * half;
                    if height >= 0.5 {
                        recorded.add_colored_rect(bar(x, height), color.gamma_multiply(0.3));
                    }
                    let height = waveform_height(peak * row.volume) * half;
                    if height >= 0.5 {
                        adjusted.add_colored_rect(bar(x, height), color);
                    }
                }
                painter.add(egui::Shape::mesh(recorded));
                painter.add(egui::Shape::mesh(adjusted));
            }
        }
        if gains_changed {
            if let Some(player) = &self.player {
                player.set_gains(&self.mix());
            }
            if matches!(self.saving, Saving::Done(_)) {
                self.saving = Saving::Idle;
            }
        }

        if duration > 0 {
            let x = x_of(self.position().min(duration));
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, visuals.strong_text_color()));
        }
    }
}

fn open_preview(ctx: &egui::Context, path: &Path) -> VideoPreview {
    let repaint = ctx.clone();
    VideoPreview::open(path, PREVIEW_MAX_WIDTH, move || repaint.request_repaint())
}

/// Peak level per [`PEAK_FRAMES`], both channels; empty for a silent track.
fn peaks(track: &TrackAudio) -> Vec<f32> {
    track
        .samples
        .chunks(PEAK_FRAMES * 2)
        .map(|chunk| chunk.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0)
        .collect()
}

/// 0..1 on a dB scale, so quiet audio still shows.
fn waveform_height(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + WAVEFORM_RANGE_DB) / WAVEFORM_RANGE_DB).clamp(0.0, 1.0)
}

/// `<stem> (edited).mp4`, or `(edited 2)` and so on if that's taken.
fn edited_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut candidate = dir.join(format!("{stem} (edited).mp4"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} (edited {n}).mp4"));
        n += 1;
    }
    candidate
}

fn frames_to_hns(frames: u64) -> i64 {
    (frames as i128 * HNS_PER_SECOND as i128 / SAMPLE_RATE as i128) as i64
}

fn hns_to_frames(hns: i64) -> u64 {
    (hns.max(0) as i128 * SAMPLE_RATE as i128 / HNS_PER_SECOND as i128) as u64
}

fn hns_to_duration(hns: i64) -> Duration {
    Duration::from_nanos(hns.max(0) as u64 * 100)
}

/// "0:12.3" / "1:05.0".
fn format_duration_precise(hns: i64) -> String {
    let tenths = hns.max(0) / (HNS_PER_SECOND / 10);
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

/// Plays the edited mix of a clip's tracks on the default output device.
/// Its position is the audio actually heard, which the video follows.
struct Player {
    shared: Arc<PlayerShared>,
    worker: Option<JoinHandle<()>>,
}

struct PlayerShared {
    audio: Arc<ClipAudio>,
    /// Each track's weight in the mix.
    gains: Vec<AtomicF32>,
    playing: AtomicBool,
    seek: Mutex<Option<u64>>,
    /// Frames heard so far.
    position: AtomicU64,
    started: Instant,
    /// When the editor was last drawn (ms since `started`).
    last_seen: AtomicU64,
    stop: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Player {
    fn start(ctx: &egui::Context, audio: Arc<ClipAudio>) -> Self {
        let shared = Arc::new(PlayerShared {
            gains: audio.tracks.iter().map(|_| AtomicF32::new(0.0)).collect(),
            audio,
            playing: AtomicBool::new(false),
            seek: Mutex::new(None),
            position: AtomicU64::new(0),
            started: Instant::now(),
            last_seen: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let (worker_shared, repaint) = (shared.clone(), ctx.clone());
        let worker = std::thread::Builder::new()
            .name("clip-playback".into())
            .spawn(move || {
                if let Err(error) = playback(&worker_shared) {
                    *worker_shared.error.lock().unwrap() = Some(error.message());
                    repaint.request_repaint();
                }
            })
            .expect("spawn clip playback thread");
        Self { shared, worker: Some(worker) }
    }

    fn position(&self) -> u64 {
        self.shared.position.load(Ordering::Relaxed)
    }

    fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    fn set_playing(&self, playing: bool) {
        self.seen();
        self.shared.playing.store(playing, Ordering::Relaxed);
    }

    fn seek(&self, frame: u64) {
        self.shared.position.store(frame, Ordering::Relaxed);
        *self.shared.seek.lock().unwrap() = Some(frame);
    }

    fn set_gains(&self, gains: &[f32]) {
        for (gain, &value) in self.shared.gains.iter().zip(gains) {
            gain.store(value);
        }
    }

    fn seen(&self) {
        self.shared.last_seen.store(self.shared.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Runs on the "clip-playback" thread: keeps a little of the mix queued on
/// the default output while playing.
fn playback(shared: &PlayerShared) -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let (client, render) = audio_engine::render::open_default_render_client(&audio_engine::loopback::standard_format())?;
    let capacity = unsafe { client.GetBufferSize()? }.min(PLAYBACK_LEAD_FRAMES);
    let total = shared.audio.frames as u64;
    let mut written = 0u64;
    let mut running = false;
    let stop_output = |running: &mut bool| -> windows::core::Result<()> {
        if std::mem::take(running) {
            unsafe { client.Stop()? };
        }
        unsafe { client.Reset() }
    };

    while !shared.stop.load(Ordering::Relaxed) {
        // Wake often enough to keep the short queue fed; less while paused.
        std::thread::sleep(Duration::from_millis(if running { 5 } else { 20 }));
        if let Some(to) = shared.seek.lock().unwrap().take() {
            stop_output(&mut running)?;
            written = to.min(total);
            shared.position.store(written, Ordering::Relaxed);
        }
        let unseen = shared.started.elapsed().as_millis() as u64 - shared.last_seen.load(Ordering::Relaxed);
        if unseen > UNSEEN_PAUSE.as_millis() as u64 {
            shared.playing.store(false, Ordering::Relaxed);
        }

        let playing = shared.playing.load(Ordering::Relaxed);
        if !playing {
            if running {
                // Resume from what was heard, not what was queued.
                let queued = unsafe { client.GetCurrentPadding()? } as u64;
                stop_output(&mut running)?;
                written = written.saturating_sub(queued);
                shared.position.store(written, Ordering::Relaxed);
            }
            continue;
        }

        let queued = if running { unsafe { client.GetCurrentPadding()? } } else { 0 };
        let heard = written.saturating_sub(queued as u64);
        shared.position.store(heard.min(total), Ordering::Relaxed);
        if heard >= total {
            stop_output(&mut running)?;
            written = total;
            shared.playing.store(false, Ordering::Relaxed);
            continue;
        }
        let room = capacity.saturating_sub(queued);
        if room > 0 {
            let data = unsafe { render.GetBuffer(room)? };
            let out = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, room as usize * 2) };
            out.fill(0.0);
            for (track, gain) in shared.audio.tracks.iter().zip(&shared.gains) {
                track.mix_into(written as usize, gain.load(), out);
            }
            out.iter_mut().for_each(|sample| *sample = sample.clamp(-1.0, 1.0));
            unsafe { render.ReleaseBuffer(room, 0)? };
            written += room as u64;
        }
        if !running {
            unsafe { client.Start()? };
            running = true;
        }
    }
    stop_output(&mut running)
}
