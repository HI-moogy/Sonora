//! Finding which window to record when clips record "games and apps you
//! choose": games are detected automatically, and you can add apps or
//! leave out a detected game.
//!
//! A program counts as a game if Windows itself recognizes it (the list
//! Game Bar keeps under `HKCU\System\GameConfigStore`) or it's installed in
//! a game launcher's library folder (Steam, Epic, Riot, EA, Ubisoft, GOG,
//! Xbox).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

/// Install folders of game launchers' libraries (lowercase path parts).
const LAUNCHER_FOLDERS: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\riot games\\",
    "\\ea games\\",
    "\\origin games\\",
    "\\ubisoft game launcher\\games\\",
    "\\gog galaxy\\games\\",
    "\\xboxgames\\",
];

/// Helpers that live in game folders but aren't the game.
const NOT_GAMES: &[&str] = &["crash", "launcher", "anticheat", "easyanticheat", "beservice", "errorreporter", "setup", "unins"];

/// How often to re-read Windows' list of games (it grows as you play new ones).
const KNOWN_GAMES_REFRESH: Duration = Duration::from_secs(60);

/// Smallest window worth recording (skips splash screens and tool popups).
const MIN_SIZE: (i32, i32) = (320, 240);

/// A running app's main window.
#[derive(Debug, Clone)]
pub struct AppWindow {
    pub window: isize,
    /// Lowercase executable name, e.g. "deadlock.exe".
    pub exe: String,
    pub title: String,
    pub is_game: bool,
}

pub struct GameFinder {
    /// Full paths (lowercase) of programs Windows recognizes as games.
    known: HashSet<String>,
    loaded_at: Instant,
}

impl GameFinder {
    pub fn new() -> Self {
        Self { known: windows_known_games(), loaded_at: Instant::now() }
    }

    fn is_game(&self, path: &str) -> bool {
        let exe = path.rsplit('\\').next().unwrap_or(path);
        if NOT_GAMES.iter().any(|part| exe.contains(part)) {
            return false;
        }
        self.known.contains(path) || LAUNCHER_FOLDERS.iter().any(|folder| path.contains(folder))
    }

    /// Every app with a window big enough to record, one per window.
    pub fn app_windows(&mut self) -> Vec<AppWindow> {
        if self.loaded_at.elapsed() >= KNOWN_GAMES_REFRESH {
            self.known = windows_known_games();
            self.loaded_at = Instant::now();
        }
        let mut found: Vec<(isize, String, String)> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect_window), LPARAM(&mut found as *mut Vec<(isize, String, String)> as isize));
        }
        found
            .into_iter()
            .map(|(window, path, title)| {
                let is_game = self.is_game(&path);
                let exe = path.rsplit('\\').next().unwrap_or(&path).to_string();
                AppWindow { window, exe, title, is_game }
            })
            .collect()
    }
}

/// Picks the window to record among `apps`: games (unless in `never`) and
/// apps in `always`. The one in front wins; otherwise keep recording
/// `current` while it's still there; otherwise any of them.
pub fn pick_window<'a>(
    apps: &'a [AppWindow],
    always: &[String],
    never: &[String],
    current: Option<isize>,
) -> Option<&'a AppWindow> {
    let wanted = |app: &&AppWindow| {
        always.contains(&app.exe) || (app.is_game && !never.contains(&app.exe))
    };
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    let candidates: Vec<&AppWindow> = apps.iter().filter(wanted).collect();
    candidates
        .iter()
        .find(|app| app.window == foreground)
        .or_else(|| candidates.iter().find(|app| Some(app.window) == current))
        .or(candidates.first())
        .copied()
}

unsafe extern "system" fn collect_window(window: HWND, lparam: LPARAM) -> BOOL {
    let found = &mut *(lparam.0 as *mut Vec<(isize, String, String)>);
    if !IsWindowVisible(window).as_bool()
        || GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid())
        || GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
    {
        return true.into();
    }
    // A minimized window reports a tiny size; keep it (the game is open).
    let mut rect = RECT::default();
    let _ = GetClientRect(window, &mut rect);
    if !IsIconic(window).as_bool() && (rect.right - rect.left < MIN_SIZE.0 || rect.bottom - rect.top < MIN_SIZE.1) {
        return true.into();
    }
    let mut title = [0u16; 256];
    let len = GetWindowTextW(window, &mut title) as usize;
    if len == 0 {
        return true.into();
    }
    let mut pid = 0;
    GetWindowThreadProcessId(window, Some(&mut pid));
    if pid == std::process::id() {
        return true.into();
    }
    if let Some(path) = process_path(pid) {
        found.push((window.0 as isize, path.to_lowercase(), String::from_utf16_lossy(&title[..len])));
    }
    true.into()
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

/// The games Windows recognizes: each subkey of GameConfigStore\Children
/// names the full path of a game's executable in `MatchedExeFullPath`.
fn windows_known_games() -> HashSet<String> {
    let mut games = HashSet::new();
    unsafe {
        let mut children = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            windows::core::w!("System\\GameConfigStore\\Children"),
            Some(0),
            KEY_READ,
            &mut children,
        )
        .is_err()
        {
            return games;
        }
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut name_len = name.len() as u32;
            if RegEnumKeyExW(children, index, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, None, None, None)
                .is_err()
            {
                break;
            }
            let mut path = [0u16; 1024];
            let mut size = (path.len() * 2) as u32;
            let read = RegGetValueW(
                children,
                windows::core::PCWSTR(name.as_ptr()),
                windows::core::w!("MatchedExeFullPath"),
                RRF_RT_REG_SZ,
                None,
                Some(path.as_mut_ptr().cast()),
                Some(&mut size),
            );
            if read.is_ok() {
                let len = (size as usize / 2).saturating_sub(1);
                games.insert(String::from_utf16_lossy(&path[..len]).to_lowercase());
            }
        }
        let _ = RegCloseKey(children);
    }
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finder(known: &[&str]) -> GameFinder {
        GameFinder { known: known.iter().map(|k| k.to_string()).collect(), loaded_at: Instant::now() }
    }

    #[test]
    fn recognizes_known_games_and_launcher_libraries() {
        let finder = finder(&["c:\\games\\deadlock\\game\\bin\\win64\\deadlock.exe"]);
        assert!(finder.is_game("c:\\games\\deadlock\\game\\bin\\win64\\deadlock.exe"));
        assert!(finder.is_game("d:\\steamlibrary\\steamapps\\common\\peak\\peak.exe"));
        assert!(finder.is_game("c:\\riot games\\valorant\\live\\valorant.exe"));
        assert!(!finder.is_game("c:\\program files\\mozilla firefox\\firefox.exe"));
    }

    #[test]
    fn helpers_in_game_folders_are_not_games() {
        let finder = finder(&[]);
        assert!(!finder.is_game("d:\\steamlibrary\\steamapps\\common\\peak\\unitycrashhandler64.exe"));
        assert!(!finder.is_game("c:\\riot games\\riot client\\riotclientservices_launcher.exe"));
    }

    fn app(window: isize, exe: &str, is_game: bool) -> AppWindow {
        AppWindow { window, exe: exe.into(), title: exe.into(), is_game }
    }

    #[test]
    fn picks_games_and_chosen_apps_but_not_left_out_games() {
        let apps = [app(1, "firefox.exe", false), app(2, "deadlock.exe", true), app(3, "cs2.exe", true)];
        // Nothing in front matches (tests run without these windows), so
        // the current target is kept while it's still a candidate.
        assert_eq!(pick_window(&apps, &[], &[], Some(3)).map(|a| a.window), Some(3));
        assert_eq!(pick_window(&apps, &[], &["cs2.exe".into()], Some(3)).map(|a| a.window), Some(2));
        assert_eq!(pick_window(&apps[..1], &[], &[], None).map(|a| a.window), None);
        assert_eq!(pick_window(&apps[..1], &["firefox.exe".into()], &[], None).map(|a| a.window), Some(1));
    }
}
