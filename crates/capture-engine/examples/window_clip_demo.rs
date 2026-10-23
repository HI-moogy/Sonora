//! Records one window (found by part of its title) instead of the whole
//! screen, saves a clip, then switches to recording nothing and checks the
//! buffer empties once the clip length has passed.
//! Usage: window_clip_demo <part of a window title> [seconds to record]
//! Clips go to CLIP_DIR if set, else the default clips folder.

use std::time::Duration;

use capture_engine::{default_clips_dir, CaptureTarget, Recorder, Resolution, VideoSettings};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};

fn find_window(part: &str) -> Option<(isize, String)> {
    struct Search<'a>(&'a str, Option<(isize, String)>);
    unsafe extern "system" fn check(window: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        let mut title = [0u16; 256];
        let len = GetWindowTextW(window, &mut title) as usize;
        let title = String::from_utf16_lossy(&title[..len]);
        if IsWindowVisible(window).as_bool() && title.contains(search.0) {
            search.1 = Some((window.0 as isize, title));
            return false.into();
        }
        true.into()
    }
    let mut search = Search(part, None);
    unsafe {
        let _ = EnumWindows(Some(check), LPARAM(&mut search as *mut Search as isize));
    }
    search.1
}

fn main() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.expect("set DPI awareness");
    let mut args = std::env::args().skip(1);
    let part = args.next().expect("part of a window title");
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);
    let (window, title) = find_window(&part).expect("no visible window with that title");
    println!("window: {title}");

    let settings = VideoSettings { buffer_secs: 4, fps: 60, resolution: Resolution::P1080 };
    let recorder = Recorder::start(settings, &["Game"]).expect("start recorder");
    recorder.set_target(CaptureTarget::Window(window));
    std::thread::sleep(Duration::from_secs(seconds));
    let status = recorder.status();
    println!("recording window: buffered {:.1}s, error {:?}", status.buffered().as_secs_f32(), status.error());
    let dir = std::env::var_os("CLIP_DIR").map(std::path::PathBuf::from).unwrap_or_else(default_clips_dir);
    match recorder.save_clip(&dir).recv().expect("save reply") {
        Ok(path) => println!("saved {}", path.display()),
        Err(error) => println!("save failed: {error}"),
    }

    recorder.set_target(CaptureTarget::Nothing);
    std::thread::sleep(Duration::from_secs(2));
    match recorder.save_clip(&dir).recv().expect("save reply") {
        Ok(path) => println!("2 s after stopping, a clip still saves: {}", path.display()),
        Err(error) => println!("2 s after stopping, save failed: {error}"),
    }
    std::thread::sleep(Duration::from_secs(settings.buffer_secs as u64 + 1));
    match recorder.save_clip(&dir).recv().expect("save reply") {
        Ok(path) => println!("after the clip length passed, a clip still saved (unexpected): {}", path.display()),
        Err(error) => println!("after the clip length passed, nothing to save: {error}"),
    }
}
