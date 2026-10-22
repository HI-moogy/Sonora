use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use windows::core::{HSTRING, PWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};

pub struct ClipEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub duration: Option<Duration>,
}

/// Saved clips in a folder, newest first. Durations come from each file's
/// MP4 header and are cached until the file changes.
#[derive(Default)]
pub struct ClipList {
    pub entries: Vec<ClipEntry>,
    durations: HashMap<PathBuf, (SystemTime, Option<Duration>)>,
}

impl ClipList {
    pub fn refresh(&mut self, dir: &Path) {
        let mut found: Vec<(PathBuf, SystemTime, u64)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4")))
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((entry.path(), meta.modified().ok()?, meta.len()))
            })
            .collect();
        found.sort_by_key(|&(_, modified, _)| std::cmp::Reverse(modified));

        self.durations.retain(|path, _| found.iter().any(|(p, ..)| p == path));
        self.entries = found
            .into_iter()
            .map(|(path, modified, size)| {
                let duration = match self.durations.get(&path) {
                    Some(&(cached_at, duration)) if cached_at == modified => duration,
                    _ => {
                        let duration = capture_engine::clip_duration(&path);
                        self.durations.insert(path.clone(), (modified, duration));
                        duration
                    }
                };
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                ClipEntry { path, name, size, duration }
            })
            .collect();
    }
}

/// Opens a clip in the default video player.
pub fn play(path: &Path) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Opens File Explorer with the clip selected.
pub fn show_in_folder(path: &Path) {
    // Explorer wants `/select,"path"` as one argument with the quotes inside
    // it, which Command's own quoting would wrap differently.
    let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

pub fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = Command::new("explorer").arg(dir).spawn();
}

/// Shows the standard Windows folder picker starting at `start`. Blocks
/// until the user picks a folder (`Some`) or cancels (`None`).
pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&folder);
        }
        dialog.Show(None).ok()?;
        let path: PWSTR = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let picked = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        picked.map(PathBuf::from)
    }
}

/// "1:05" / "12:30".
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64().round() as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    }
}
