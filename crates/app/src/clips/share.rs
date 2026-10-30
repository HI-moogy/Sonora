//! Share to Discord: copies of a clip that each fit Discord's upload limit
//! (see [`capture_engine::share_clip`]), copied so they paste into a chat.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use capture_engine::{plan_share, share_clip, SharePlan, ShareSource};
use eframe::egui;
use windows::Win32::Foundation::{GlobalFree, HANDLE, POINT};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::DROPFILES;

use super::{display_name, format_duration, format_size, show_in_folder};
use crate::{theme, FIXED_BUSES};

/// Where in the clips folder the copies go; the clip list leaves it out.
pub const FOLDER: &str = "For Discord";

/// Limits to pick from, in MB: Discord's for free accounts and Nitro's.
const LIMITS: [(u32, &str); 3] = [(20, "Discord, free"), (50, "Nitro Basic"), (1000, "Nitro")];

/// How much of each audio track a copy's one track gets: what you heard and
/// what you said. A Sonora clip's Desktop and Mic tracks; an edited clip's
/// mix, written last; another recorder's game and mic tracks; otherwise
/// the track players play, the last.
pub fn share_mix(tracks: usize) -> Vec<f32> {
    let mut mix = vec![0.0; tracks];
    let buses = FIXED_BUSES.len();
    if tracks == buses + 1 {
        mix[buses] = 1.0;
        if let Some(mic) = FIXED_BUSES.iter().position(|bus| *bus == "Mic") {
            mix[mic] = 1.0;
        }
    } else if tracks <= 2 {
        mix.fill(1.0);
    } else if let Some(last) = mix.last_mut() {
        *last = 1.0;
    }
    mix
}

/// The Share to Discord window for one clip.
pub struct ShareWindow {
    clip: PathBuf,
    name: String,
    /// What the clip is, or why it can't be shared.
    source: Result<ShareSource, String>,
    job: Option<Job>,
    /// The copies made, or why they couldn't be.
    made: Option<Result<Vec<PathBuf>, String>>,
    /// The size limit the copies were made for, in MB.
    made_for: u32,
    /// Whether copying them worked.
    copied: Option<Result<(), String>>,
}

/// Copies being made on a background thread.
struct Job {
    /// How far along (f32 bits, 0 to 1).
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: Receiver<Result<Vec<PathBuf>, String>>,
}

impl ShareWindow {
    pub fn open(clip: &Path, name: &str) -> Self {
        Self {
            clip: clip.to_path_buf(),
            name: name.to_string(),
            source: ShareSource::read(clip).map_err(|error| error.to_string()),
            job: None,
            made: None,
            made_for: 0,
            copied: None,
        }
    }

    /// Draws the window; `limit_mb` is the size limit, which you can change
    /// here. Returns whether it's still open, and whether `limit_mb` was
    /// changed and settled (to save).
    pub fn show(&mut self, ctx: &egui::Context, limit_mb: &mut u32, clips_dir: &Path) -> (bool, bool) {
        self.poll();
        let mut open = true;
        let mut closed = false;
        let mut limit_changed = false;
        egui::Window::new("Share to Discord")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(egui::RichText::new(display_name(&self.name)).strong());
                let source = match &self.source {
                    Ok(source) => source.clone(),
                    Err(error) => {
                        ui.colored_label(theme::danger(), format!("Sonora can't share this clip: {error}"));
                        return;
                    }
                };
                ui.add_space(6.0);
                ui.add_enabled_ui(self.job.is_none(), |ui| limit_changed = limit_picker(ui, limit_mb));
                let limit = u64::from(*limit_mb) * 1_000_000;
                ui.label(egui::RichText::new(describe(&plan_share(&source, limit))).color(theme::text_muted()));
                ui.add_space(8.0);

                if let Some(job) = &self.job {
                    let progress = f32::from_bits(job.progress.load(Ordering::Relaxed));
                    ui.add(egui::ProgressBar::new(progress).show_percentage());
                    if ui.button("Stop").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                    return;
                }
                match &self.made {
                    Some(Ok(copies)) if self.made_for == *limit_mb => closed = self.made_ui(ui, copies.clone()),
                    Some(Err(error)) if self.made_for == *limit_mb => {
                        ui.colored_label(theme::danger(), format!("Couldn't make the copies: {error}"));
                        if ui.button("Try again").clicked() {
                            self.start(ctx, clips_dir, *limit_mb);
                        }
                    }
                    // Not made yet, or for another limit.
                    _ => {
                        if ui.add(theme::primary_button("Make the copies")).clicked() {
                            self.start(ctx, clips_dir, *limit_mb);
                        }
                    }
                }
            });
        if !open || closed {
            if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
        (open && !closed, limit_changed)
    }

    /// The copies made, and what to do with them. Returns whether to close.
    fn made_ui(&mut self, ui: &mut egui::Ui, copies: Vec<PathBuf>) -> bool {
        ui.label(format!("Ready: {} file{}", copies.len(), if copies.len() == 1 { "" } else { "s" }));
        for copy in &copies {
            let name = copy.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let size = std::fs::metadata(copy).map_or(0, |meta| meta.len());
            ui.label(egui::RichText::new(format!("{name} · {}", format_size(size))).small().color(theme::text_muted()));
        }
        if copies.len() > 10 {
            ui.label(egui::RichText::new("Discord takes up to 10 files per message.").small().color(theme::warning()));
        }
        ui.add_space(6.0);
        let mut close = false;
        ui.horizontal(|ui| {
            if ui.add(theme::primary_button("Copy")).on_hover_text("Then paste them in a Discord chat").clicked() {
                self.copied = Some(copy_files(&copies));
            }
            if ui.button("Show in folder").clicked() {
                if let Some(first) = copies.first() {
                    show_in_folder(first);
                }
            }
            close = ui.button("Done").clicked();
        });
        match &self.copied {
            Some(Ok(())) => {
                ui.label(egui::RichText::new("Copied. Paste them in a Discord chat with Ctrl+V.").color(theme::accent()));
            }
            Some(Err(error)) => {
                ui.colored_label(theme::danger(), format!("Couldn't copy them: {error}"));
            }
            None => {}
        }
        close
    }

    fn start(&mut self, ctx: &egui::Context, clips_dir: &Path, limit_mb: u32) {
        let Ok(source) = &self.source else { return };
        let mix = share_mix(source.audio_tracks);
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        let (done_tx, done) = mpsc::channel();
        let (clip, folder, repaint) = (self.clip.clone(), clips_dir.join(FOLDER), ctx.clone());
        let (thread_progress, thread_cancel) = (progress.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("clip-share".into())
            .spawn(move || {
                let report = |done: f32| {
                    thread_progress.store(done.to_bits(), Ordering::Relaxed);
                    repaint.request_repaint_after(Duration::from_millis(100));
                };
                let limit = u64::from(limit_mb) * 1_000_000;
                let made = share_clip(&clip, &folder, limit, &mix, &report, &thread_cancel);
                let _ = done_tx.send(made.map_err(|error| error.to_string()));
                repaint.request_repaint();
            })
            .expect("spawn clip share thread");
        self.job = Some(Job { progress, cancel, done });
        self.made = None;
        self.made_for = limit_mb;
        self.copied = None;
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let made = match job.done.try_recv() {
            Ok(made) => made,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the copying stopped".to_string()),
        };
        let stopped = job.cancel.load(Ordering::Relaxed);
        self.job = None;
        // Stopped by you: back to the start, not an error.
        self.made = (!stopped).then_some(made);
    }
}

/// The size limit: Discord's limits to pick from, or any. Returns whether
/// it changed and settled.
fn limit_picker(ui: &mut egui::Ui, limit_mb: &mut u32) -> bool {
    let mut settled = false;
    ui.horizontal(|ui| {
        ui.label("Each file under");
        let label = |mb: u32| if mb >= 1000 && mb % 1000 == 0 { format!("{} GB", mb / 1000) } else { format!("{mb} MB") };
        let selected = match LIMITS.iter().find(|(mb, _)| mb == limit_mb) {
            Some((mb, what)) => format!("{} ({what})", label(*mb)),
            None => "Other".to_string(),
        };
        egui::ComboBox::from_id_salt("share-limit").width(170.0).selected_text(selected).show_ui(ui, |ui| {
            for (mb, what) in LIMITS {
                if ui.selectable_label(*limit_mb == mb, format!("{} ({what})", label(mb))).clicked() {
                    *limit_mb = mb;
                    settled = true;
                }
            }
        });
        let typed = ui.add(egui::DragValue::new(limit_mb).range(1..=10_000).suffix(" MB").speed(1.0));
        settled |= typed.changed() && !typed.dragged() || typed.drag_stopped();
    });
    settled
}

/// What the copies will be, in a sentence.
fn describe(plan: &SharePlan) -> String {
    match plan {
        SharePlan::AsIs => "It fits as it is: the same picture, with its sound in one track.".into(),
        SharePlan::Encode(plan) => {
            let quality = format!("{}p at {} fps", plan.width.min(plan.height), plan.fps);
            match plan.parts.as_slice() {
                [_] => format!("Made smaller to fit: {quality}."),
                parts => {
                    let length = parts.first().map_or(0, |&(start, end)| end - start);
                    format!(
                        "Too long for one file at a good quality: {} files of {} each, {quality}.",
                        parts.len(),
                        format_duration(Duration::from_nanos(length as u64 * 100))
                    )
                }
            }
        }
    }
}

/// Puts `files` on the clipboard as files, as Explorer's Copy does, so
/// pasting in Discord attaches them.
fn copy_files(files: &[PathBuf]) -> Result<(), String> {
    // DROPFILES, then each path, each ending in a nul, and one more nul.
    let mut paths: Vec<u16> = Vec::new();
    for file in files {
        paths.extend(file.as_os_str().encode_wide());
        paths.push(0);
    }
    paths.push(0);
    let header = size_of::<DROPFILES>();
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, header + paths.len() * 2).map_err(|error| error.to_string())?;
        let data = GlobalLock(memory) as *mut u8;
        if data.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("out of memory".into());
        }
        let list = DROPFILES { pFiles: header as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        std::ptr::write_unaligned(data as *mut DROPFILES, list);
        std::ptr::copy_nonoverlapping(paths.as_ptr() as *const u8, data.add(header), paths.len() * 2);
        let _ = GlobalUnlock(memory);
        if let Err(error) = OpenClipboard(None) {
            let _ = GlobalFree(Some(memory));
            return Err(error.to_string());
        }
        // Once set, the clipboard owns the memory.
        let set = EmptyClipboard().and_then(|()| SetClipboardData(u32::from(CF_HDROP.0), Some(HANDLE(memory.0))));
        let _ = CloseClipboard();
        if let Err(error) = set {
            let _ = GlobalFree(Some(memory));
            return Err(error.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_have_what_you_heard_and_said() {
        assert_eq!(share_mix(FIXED_BUSES.len() + 1), [0.0, 0.0, 0.0, 0.0, 1.0, 1.0], "Desktop and Mic");
        assert_eq!(share_mix(FIXED_BUSES.len() + 2).last(), Some(&1.0), "an edited clip's mix");
        assert_eq!(share_mix(FIXED_BUSES.len() + 2).iter().sum::<f32>(), 1.0);
        assert_eq!(share_mix(2), [1.0, 1.0], "another recorder's game and mic");
    }
}
