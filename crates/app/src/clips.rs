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
    pub modified: SystemTime,
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
                ClipEntry { path, name, modified, size, duration }
            })
            .collect();
    }
}

/// Clip thumbnails from Windows' own thumbnail cache (the same images File
/// Explorer shows), loaded on a background thread so a big folder never
/// stalls the UI.
pub struct Thumbnails {
    textures: HashMap<PathBuf, (SystemTime, Option<egui::TextureHandle>)>,
    requested: HashSet<PathBuf>,
    requests: mpsc::Sender<(PathBuf, SystemTime)>,
    results: mpsc::Receiver<(PathBuf, SystemTime, Option<egui::ColorImage>)>,
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
                    let image = thumbnail(&path);
                    if result_tx.send((path, modified, image)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn thumbnail thread");
        Self { textures: HashMap::new(), requested: HashSet::new(), requests, results }
    }

    /// The clip's thumbnail if it's loaded; otherwise asks for it and
    /// returns `None` for now.
    pub fn get(&mut self, ctx: &egui::Context, clip: &ClipEntry) -> Option<egui::TextureHandle> {
        while let Ok((path, modified, image)) = self.results.try_recv() {
            self.requested.remove(&path);
            let texture = image.map(|image| ctx.load_texture(path.display().to_string(), image, Default::default()));
            self.textures.insert(path, (modified, texture));
        }
        match self.textures.get(&clip.path) {
            Some((modified, texture)) if *modified == clip.modified => texture.clone(),
            _ => {
                if self.requested.insert(clip.path.clone()) {
                    let _ = self.requests.send((clip.path.clone(), clip.modified));
                }
                None
            }
        }
    }

    /// Frees thumbnails of clips that are no longer in the list.
    pub fn retain(&mut self, clips: &[ClipEntry]) {
        self.textures.retain(|path, _| clips.iter().any(|clip| &clip.path == path));
    }
}

fn thumbnail(path: &Path) -> Option<egui::ColorImage> {
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let [cx, cy] = THUMBNAIL_SIZE;
        let bitmap = factory.GetImage(SIZE { cx, cy }, SIIGBF_THUMBNAILONLY).ok()?;

        let mut info = BITMAP::default();
        let read = GetObjectW(bitmap.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as *mut _));
        let image = (read != 0).then(|| {
            let (width, height) = (info.bmWidth, info.bmHeight);
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

/// Opens `url` in the default browser.
pub fn open_url(url: &str) {
    let _ = Command::new("explorer").arg(url).spawn();
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
