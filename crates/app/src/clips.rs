use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use eframe::egui;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName,
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH, SIIGBF_THUMBNAILONLY,
};

pub mod editor;

/// Thumbnails are requested at this size; clips are always 16:9.
pub const THUMBNAIL_SIZE: [i32; 2] = [320, 180];

pub struct ClipEntry {
    pub path: PathBuf,
    pub name: String,
    /// The game the clip is from: its folder's name, or read from its file
    /// name (see [`game_of`]).
    pub game: String,
    /// Whether `game` is the clip's folder's name (rather than read from
    /// its file name); folder names are what you chose, so they win when
    /// both spell a game.
    pub game_from_folder: bool,
    /// When it was recorded, as local YYYYMMDDHHMMSS: from its file name
    /// when that has a date (recorders put one there; copying a file
    /// changes its modified date), else the file's modified date.
    pub recorded: u64,
    pub modified: SystemTime,
    pub size: u64,
}

/// Every clip under the clips folder, including its subfolders (one per
/// game, say), found on a background thread: a big library on a hard drive
/// takes a while to walk and must never stall the window.
pub struct ClipList {
    pub entries: Vec<ClipEntry>,
    requests: mpsc::Sender<PathBuf>,
    results: mpsc::Receiver<Vec<ClipEntry>>,
    /// Whether a scan was asked for and hasn't come back yet.
    pub scanning: bool,
    /// Whether any scan has come back yet.
    pub scanned: bool,
}

impl ClipList {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, request_rx) = mpsc::channel::<PathBuf>();
        let (result_tx, results) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("clip-scan".into())
            .spawn(move || {
                while let Ok(mut dir) = request_rx.recv() {
                    // Only the latest request matters.
                    while let Ok(newer) = request_rx.try_recv() {
                        dir = newer;
                    }
                    let mut found = Vec::new();
                    scan(&dir, &dir, 0, &mut found);
                    if result_tx.send(found).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn clip scan thread");
        Self { entries: Vec::new(), requests, results, scanning: false, scanned: false }
    }

    /// Starts scanning `dir` again; [`Self::poll`] picks up the result.
    pub fn refresh(&mut self, dir: &Path) {
        if self.requests.send(dir.to_path_buf()).is_ok() {
            self.scanning = true;
        }
    }

    /// Takes the latest finished scan, if any. Returns whether the list changed.
    pub fn poll(&mut self) -> bool {
        let mut latest = None;
        while let Ok(found) = self.results.try_recv() {
            latest = Some(found);
        }
        let Some(found) = latest else { return false };
        self.entries = found;
        self.scanning = false;
        self.scanned = true;
        true
    }
}

/// How deep into subfolders clips are looked for (Clips\Game\Session\x.mp4).
const MAX_DEPTH: usize = 4;

/// Folders of other recorders' caches and thumbnails, not clips.
fn is_cache_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.') || lower.contains("cache") || lower.contains("thumb")
}

fn scan(root: &Path, dir: &Path, depth: usize, found: &mut Vec<ClipEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if depth < MAX_DEPTH && !is_cache_folder(&name) {
                scan(root, &path, depth + 1, found);
            }
        } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mp4")) {
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let (game, game_from_folder) = game_of(root, &path, &name);
            let recorded = date_in_name(&name).map_or_else(|| local_timestamp(modified), |(date, _)| date);
            found.push(ClipEntry { path, name, game, game_from_folder, recorded, modified, size: meta.len() });
        }
    }
}

/// The game a clip is from. Recorders name it in the file name, which
/// wins: SteelSeries Moments' "Team-Fortress-2__2025-11-23_...", AMD
/// Radeon's "Counter-Strike 2_replay_2025.01.02-..." or "IMSCARED_2025...".
/// Otherwise (a clip you renamed, Radeon's "unknown_replay_..."), the first
/// folder under the clips folder it's in; otherwise, for a clip right in
/// the clips folder, Radeon replays (its "Replay_..." clips it couldn't tie
/// to a game), Sonora, or Other. Also returns whether the name shown is a
/// folder's, since your folder's spelling of a game wins.
fn game_of(root: &Path, path: &Path, name: &str) -> (String, bool) {
    let folder = path.strip_prefix(root).ok().and_then(|rel| {
        let mut parts = rel.components();
        let first = parts.next()?;
        parts.next().map(|_| first.as_os_str().to_string_lossy().into_owned())
    });
    match (game_in_name(name), folder) {
        (Some(game), Some(folder)) if game_key(&game) == game_key(&folder) => (folder, true),
        (Some(game), _) => (game, false),
        (None, Some(folder)) => (folder, true),
        (None, None) => {
            let lower = name.to_lowercase();
            let game = if lower.starts_with("replay") {
                "Radeon replays"
            } else if lower.starts_with("sonora") || lower.starts_with("sonar-rs") {
                "Sonora"
            } else {
                "Other"
            };
            (game.to_string(), false)
        }
    }
}

/// The game a recorder wrote into a clip's name: the part before its date
/// (or before "_replay"), if that names one.
fn game_in_name(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let end = match date_in_name(name) {
        Some((_, at)) => at,
        None => lower.find("_replay")?,
    };
    let mut game = &name[..end];
    // Files copied in by some tools start with a millisecond timestamp.
    if let Some((stamp, rest)) = game.split_once('-') {
        if stamp.len() >= 10 && stamp.chars().all(|c| c.is_ascii_digit()) {
            game = rest;
        }
    }
    let separators: &[char] = &['_', '-', '.', ' '];
    let mut game = game.trim_matches(separators);
    if game.to_lowercase().ends_with("_replay") {
        game = game[..game.len() - "_replay".len()].trim_matches(separators);
    }
    let lower = game.to_lowercase();
    if game.is_empty() || ["replay", "unknown", "sonora", "sonar-rs"].contains(&lower.as_str()) {
        return None;
    }
    // SteelSeries writes spaces as dashes ("Team-Fortress-2").
    Some(if game.contains(' ') { game.to_string() } else { game.replace('-', " ") })
}

/// The recording date in a clip's file name, as YYYYMMDDHHMMSS, and where
/// in the name it starts: the first "2025-11-23_20-30-10" (SteelSeries),
/// "2025.02.10-21.16" (Radeon) or "2026-10-23 01-02-03" (Sonora) style date
/// in it, seconds optional. A name with only a year ("..._replay_2024")
/// counts as the start of that year.
fn date_in_name(name: &str) -> Option<(u64, usize)> {
    let b = name.as_bytes();
    let digits = |at: usize, n: usize| -> Option<u64> {
        let part = b.get(at..at + n)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0, |v, d| v * 10 + u64::from(d - b'0')))
    };
    let sep = |at: usize| b.get(at).is_some_and(|c| matches!(c, b'-' | b'.' | b'_' | b' '));
    let mut year_only = None;
    for start in 0..b.len() {
        if start > 0 && b[start - 1].is_ascii_digit() {
            continue;
        }
        let Some(year) = digits(start, 4).filter(|y| (1990..=2100).contains(y)) else { continue };
        if b.get(start + 4).is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let (Some(month), Some(day)) = (sep(start + 4).then(|| digits(start + 5, 2)).flatten(), sep(start + 7).then(|| digits(start + 8, 2)).flatten()) else {
            year_only.get_or_insert((year * 10_000_000_000, start));
            continue;
        };
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            continue;
        }
        // Then, maybe, hours and minutes, and maybe seconds.
        let mut at = start + 10;
        while b.get(at) == Some(&b'_') {
            at += 1;
        }
        let time = if sep(start + 10) || at > start + 10 {
            let at = if at > start + 10 { at } else { start + 11 };
            match (digits(at, 2), sep(at + 2).then(|| digits(at + 3, 2)).flatten()) {
                (Some(h), Some(m)) if h < 24 && m < 60 => {
                    // Seconds use the same separator as the minutes; Radeon's
                    // "_01" after "21.16" marks a second clip that minute.
                    let same_sep = b.get(at + 5).is_some() && b.get(at + 5) == b.get(at + 2);
                    let s = same_sep.then(|| digits(at + 6, 2)).flatten().filter(|s| *s < 60).unwrap_or(0);
                    h * 10_000 + m * 100 + s
                }
                _ => 0,
            }
        } else {
            0
        };
        return Some((((year * 100 + month) * 100 + day) * 1_000_000 + time, start));
    }
    year_only
}

/// `time` as local YYYYMMDDHHMMSS, to compare with dates in file names.
fn local_timestamp(time: SystemTime) -> u64 {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64 / 100) + 116_444_736_000_000_000;
    let file_time = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&file_time, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return 0;
        }
    }
    let date = (u64::from(local.wYear) * 100 + u64::from(local.wMonth)) * 100 + u64::from(local.wDay);
    date * 1_000_000 + u64::from(local.wHour) * 10_000 + u64::from(local.wMinute) * 100 + u64::from(local.wSecond)
}

/// A key that treats "Counter-Strike 2" and "Counter Strike 2" as the same
/// game.
pub fn game_key(game: &str) -> String {
    game.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How many thumbnails are kept in memory; the rest are reloaded when
/// scrolled back into view (each is about 230 KB).
const MAX_THUMBNAILS: usize = 150;

struct Details {
    modified: SystemTime,
    texture: Option<egui::TextureHandle>,
    duration: Option<Duration>,
    last_used: u64,
}

/// Thumbnails (from Windows' own thumbnail cache, the images File Explorer
/// shows) and lengths of the clips on screen, loaded on a background thread
/// so a big folder never stalls the window. Only clips actually shown are
/// loaded, and only the most recently shown are kept.
pub struct Thumbnails {
    details: HashMap<PathBuf, Details>,
    requested: HashSet<PathBuf>,
    requests: mpsc::Sender<(PathBuf, SystemTime)>,
    results: mpsc::Receiver<(PathBuf, SystemTime, Option<egui::ColorImage>, Option<Duration>)>,
    frame: u64,
}

impl Thumbnails {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, request_rx) = mpsc::channel::<(PathBuf, SystemTime)>();
        let (result_tx, results) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("clip-thumbnails".into())
            .spawn(move || {
                // Shell thumbnail providers expect a single-threaded apartment.
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                for (path, modified) in request_rx {
                    // A damaged file (a recorder's interrupted save) can make
                    // the MP4 reader panic: it gets no thumbnail, and the
                    // clips after it still do.
                    let (image, duration) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        (thumbnail(&path), capture_engine::clip_duration(&path))
                    }))
                    .unwrap_or((None, None));
                    if result_tx.send((path, modified, image, duration)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn thumbnail thread");
        Self { details: HashMap::new(), requested: HashSet::new(), requests, results, frame: 0 }
    }

    /// Call once per frame before [`Self::get`]: takes finished loads and
    /// frees thumbnails that haven't been shown for a while.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok((path, modified, image, duration)) = self.results.try_recv() {
            self.requested.remove(&path);
            let texture = image.map(|image| ctx.load_texture(path.display().to_string(), image, Default::default()));
            self.details.insert(path, Details { modified, texture, duration, last_used: self.frame });
        }
        if self.details.len() > MAX_THUMBNAILS {
            let mut by_age: Vec<(u64, PathBuf)> = self.details.iter().map(|(p, d)| (d.last_used, p.clone())).collect();
            by_age.sort();
            for (_, path) in by_age.into_iter().take(self.details.len() - MAX_THUMBNAILS) {
                self.details.remove(&path);
            }
        }
    }

    /// The clip's thumbnail and length, once loaded; asks for them and
    /// returns `(None, None)` meanwhile. Only call this for clips on screen.
    pub fn get(&mut self, clip: &ClipEntry) -> (Option<egui::TextureHandle>, Option<Duration>) {
        match self.details.get_mut(&clip.path) {
            Some(details) if details.modified == clip.modified => {
                details.last_used = self.frame;
                (details.texture.clone(), details.duration)
            }
            _ => {
                if self.requested.insert(clip.path.clone()) {
                    let _ = self.requests.send((clip.path.clone(), clip.modified));
                }
                (None, None)
            }
        }
    }
}

/// The largest thumbnail side accepted from a shell thumbnail provider.
const MAX_THUMBNAIL_SIDE: i32 = 2048;

fn thumbnail(path: &Path) -> Option<egui::ColorImage> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let [cx, cy] = THUMBNAIL_SIZE;
        let bitmap = factory.GetImage(SIZE { cx, cy }, SIIGBF_THUMBNAILONLY).ok()?;

        let mut info = BITMAP::default();
        let read = GetObjectW(bitmap.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as *mut _));
        // The provider says how big its bitmap is (rows may run either
        // way); anything far past what was asked for isn't allocated.
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        let sane = (1..=MAX_THUMBNAIL_SIDE).contains(&width) && (1..=MAX_THUMBNAIL_SIDE).contains(&height);
        let image = (read != 0 && sane).then(|| {
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            let dc = GetDC(None);
            let lines = GetDIBits(
                dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut header,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, dc);
            (lines != 0).then(|| {
                // BGRA to RGBA. Video thumbnails are opaque but often leave
                // the alpha byte at zero.
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                    pixel[3] = 255;
                }
                egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &pixels)
            })
        });
        let _ = DeleteObject(bitmap.into());
        image.flatten()
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

/// A clip's file name without the app name in front, for showing; clips
/// saved before the rename start with "sonar-rs ".
pub fn display_name(name: &str) -> &str {
    name.strip_prefix("Sonora ").or_else(|| name.strip_prefix("sonar-rs ")).unwrap_or(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn game(rel: &str) -> String {
        let root = Path::new(r"E:\Clips");
        let path = root.join(rel);
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        game_of(root, &path, &name).0
    }

    #[test]
    fn games_come_from_folders_then_file_names() {
        assert_eq!(game(r"Counter-Strike 2\Replay_2025.01.02-20.00.mp4"), "Counter-Strike 2");
        assert_eq!(game(r"Deadlock\session\clip.mp4"), "Deadlock");
        assert_eq!(game(r"Garry's Mod\watch yo tone.mp4"), "Garry's Mod");
        assert_eq!(game(r"unknown\unknown_replay_2024.01.01-10.00.mp4"), "unknown");
        assert_eq!(game("Team-Fortress-2__2025-11-23_20-30-10.mp4"), "Team Fortress 2");
        assert_eq!(game("Replay_2025.02.10-21.16.mp4"), "Radeon replays");
        assert_eq!(game("Sonora 2026-10-23 01-02-03.mp4"), "Sonora");
        assert_eq!(game("2024.11.27-00.16.mp4"), "Other");
    }

    #[test]
    fn dates_are_read_from_each_recorders_names() {
        assert_eq!(date_in_name("Team-Fortress-2__2025-11-23_20-30-10").map(|d| d.0), Some(20251123203010));
        assert_eq!(date_in_name("Replay_2025.02.10-21.16").map(|d| d.0), Some(20250210211600));
        assert_eq!(date_in_name("Replay_2025.01.15-01.47_01").map(|d| d.0), Some(20250115014700));
        assert_eq!(date_in_name("Sonora 2026-10-23 01-02-03").map(|d| d.0), Some(20261023010203));
        assert_eq!(date_in_name("2024.11.27-00.16").map(|d| d.0), Some(20241127001600));
        assert_eq!(date_in_name("PEAK__2025-12-03__16-42-00").map(|d| d.0), Some(20251203164200));
        assert_eq!(date_in_name("my clip"), None);
        assert_eq!(date_in_name("12025-13-40"), None);
    }

    #[test]
    fn a_game_in_the_name_beats_the_folder() {
        assert_eq!(game(r"Deadlock\Counter-Strike-2__2026-04-03__15-04-33.mp4"), "Counter Strike 2");
        assert_eq!(game(r"Deadlock\Minecraft-for-Windows__2026-03-25__22-00-00.mp4"), "Minecraft for Windows");
        assert_eq!(game(r"Imported\1778029526230-Counter-Strike 2_replay_2024.mp4"), "Counter-Strike 2");
        assert_eq!(game(r"Edits\Deadlock_replay_2025-tr-edit.mp4"), "Deadlock");
        assert_eq!(game("IMSCARED_2025.01.01-10.00.mp4"), "IMSCARED");
        assert_eq!(game(r"Counter-Strike 2\Counter-Strike 2_replay_2025.01.02-20.00.mp4"), "Counter-Strike 2");
        // The folder's spelling wins when both name the same game.
        assert_eq!(game(r"Counter-Strike 2\Counter-Strike-2__2025-12-06__18-58-53.mp4"), "Counter-Strike 2");
    }

    #[test]
    fn a_year_alone_dates_a_clip_to_the_start_of_that_year() {
        assert_eq!(date_in_name("1778029526230-Counter-Strike 2_replay_2024").map(|d| d.0), Some(20240000000000));
        assert_eq!(date_in_name("Deadlock_replay_2025-tr-edit").map(|d| d.0), Some(20250000000000));
        assert_eq!(date_in_name("watch yo tone"), None);
    }

    #[test]
    fn spelling_differences_are_the_same_game() {
        assert_eq!(game_key("Counter-Strike 2"), game_key("Counter Strike 2"));
        assert_ne!(game_key("Counter-Strike 2"), game_key("Counter-Strike Global Offensive"));
    }

    #[test]
    fn other_recorders_caches_are_skipped() {
        for name in ["thumbs", "replay_cache", ".Thumbnails", "thumb_cache", "InGame_replay_cache"] {
            assert!(is_cache_folder(name), "{name}");
        }
        for name in ["Counter-Strike 2", "Edits", "Imported", "Shared Clips"] {
            assert!(!is_cache_folder(name), "{name}");
        }
    }
}
