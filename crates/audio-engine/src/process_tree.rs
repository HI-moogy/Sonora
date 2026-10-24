//! Which processes make up an app's audio.
//!
//! Apps often play audio from a helper process rather than themselves:
//! WebView2 and Chromium-based apps play from a browser audio-service child.
//! Measured with Cider (a WebView2 app): Windows' per-app capture of Cider
//! itself, which is meant to include its child processes, returned silence,
//! while capturing its WebView2 browser process returned the music. So an
//! app's audio is followed into the descendants that own audio sessions.
//!
//! Not every descendant is a helper, though: Steam starts the games you
//! play, so once Steam itself had a sound session (voice chat, a
//! notification), counting its descendants as Steam hid the game from the
//! app list. A process with a window of its own is a program of its own,
//! so the list doesn't count it, or anything it started, as part of the
//! process above. Routing an app still takes all the audio started under
//! it, games included: Windows' per-app capture can't leave part of a
//! process tree out.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

use windows::core::{Result, BOOL};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE,
    GW_OWNER, WS_EX_TOOLWINDOW,
};

/// Running processes and the parent each was started by.
pub struct ProcessTree {
    parents: HashMap<u32, u32>,
    /// Each process's executable name (e.g. "deadlock.exe") as the process
    /// list gives it, so it's known even for a process that can't be
    /// opened (one run as administrator, say).
    names: HashMap<u32, String>,
    started: RefCell<HashMap<u32, Option<u64>>>,
    /// Processes with a window of their own, looked up when first needed.
    windowed: OnceCell<HashSet<u32>>,
}

/// Calls `visit` with every running process.
fn visit_processes(mut visit: impl FnMut(&PROCESSENTRY32W)) -> Result<()> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            visit(&entry);
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(())
}

fn exe_name(entry: &PROCESSENTRY32W) -> String {
    let len = entry
        .szExeFile
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..len])
}

/// The executable names (lowercase, e.g. "cider.exe") of every running
/// process.
pub fn running_executables() -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    visit_processes(|entry| {
        names.insert(exe_name(entry).to_lowercase());
    })?;
    Ok(names)
}

/// `pid`'s executable name from the process list, which needs no access
/// to the process itself.
pub fn listed_executable_name(pid: u32) -> Option<String> {
    let mut name = None;
    visit_processes(|entry| {
        if entry.th32ProcessID == pid {
            name = Some(exe_name(entry));
        }
    })
    .ok()?;
    name
}

impl ProcessTree {
    pub fn snapshot() -> Result<Self> {
        let mut parents = HashMap::new();
        let mut names = HashMap::new();
        visit_processes(|entry| {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            names.insert(entry.th32ProcessID, exe_name(entry));
        })?;
        Ok(Self {
            parents,
            names,
            started: RefCell::default(),
            windowed: OnceCell::new(),
        })
    }

    /// `pid`'s executable name, e.g. "deadlock.exe".
    pub fn executable_name(&self, pid: u32) -> Option<&str> {
        self.names.get(&pid).map(String::as_str)
    }

    /// `pid`'s parent, if that is still the process that started it. A
    /// recorded parent id may since have exited and been reused by a newer
    /// process, which can't be the parent; one that can't be checked (e.g.
    /// a protected process) isn't treated as the parent either.
    fn parent(&self, pid: u32) -> Option<u32> {
        let parent = *self.parents.get(&pid)?;
        if parent == 0 || parent == pid {
            return None;
        }
        let (parent_started, started) = (self.creation_time(parent)?, self.creation_time(pid)?);
        (parent_started <= started).then_some(parent)
    }

    /// `pid`'s ancestors, nearest first.
    fn ancestors(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        let mut seen = HashSet::from([pid]);
        std::iter::successors(self.parent(pid), move |&p| self.parent(p))
            .take_while(move |&p| seen.insert(p))
    }

    fn creation_time(&self, pid: u32) -> Option<u64> {
        *self
            .started
            .borrow_mut()
            .entry(pid)
            .or_insert_with(|| creation_time(pid))
    }

    fn is_descendant(&self, pid: u32, ancestor: u32) -> bool {
        self.ancestors(pid).any(|p| p == ancestor)
    }

    fn has_window(&self, pid: u32) -> bool {
        self.windowed.get_or_init(windowed_processes).contains(&pid)
    }

    /// The app each audio-session owner in `owners` belongs to: its
    /// furthest ancestor that also owns a session, or itself, counting
    /// nothing above a program of its own (see the module docs).
    pub fn owning_apps(&self, owners: &HashSet<u32>) -> HashMap<u32, u32> {
        owners
            .iter()
            .map(|&pid| (pid, self.owning_app(pid, owners)))
            .collect()
    }

    fn owning_app(&self, pid: u32, owners: &HashSet<u32>) -> u32 {
        let mut app = pid;
        // The processes climbed through since the last owner. Windows are
        // only looked up once there's an owner above them to join.
        let mut below = vec![pid];
        for ancestor in self.ancestors(pid) {
            if owners.contains(&ancestor) {
                if below.iter().any(|&p| self.has_window(p)) {
                    break;
                }
                app = ancestor;
                below.clear();
            }
            below.push(ancestor);
        }
        app
    }

    /// The audio-session owners making up `app`'s audio: `app` and its
    /// descendants in `owners`.
    pub fn app_audio_processes(&self, app: u32, owners: &HashSet<u32>) -> HashSet<u32> {
        owners
            .iter()
            .copied()
            .filter(|&pid| pid == app || self.is_descendant(pid, app))
            .chain(std::iter::once(app))
            .collect()
    }

    /// The process to capture `app`'s audio from (capture includes the
    /// process's descendants): the nearest common ancestor of its
    /// descendants that play audio, or `app` itself when none do.
    pub fn capture_target(&self, app: u32, owners: &HashSet<u32>) -> u32 {
        let helpers: Vec<u32> = owners
            .iter()
            .copied()
            .filter(|&pid| pid != app && self.is_descendant(pid, app))
            .collect();
        let Some(&first) = helpers.first() else {
            return app;
        };
        std::iter::once(first)
            .chain(self.ancestors(first))
            .take_while(|&p| p != app)
            .find(|&candidate| {
                helpers
                    .iter()
                    .all(|&h| h == candidate || self.is_descendant(h, candidate))
            })
            .unwrap_or(app)
    }
}

/// Processes showing a window of their own: one that could be on the
/// taskbar (visible, not owned by another window, not a tool window), as
/// games and apps have and helper processes don't.
fn windowed_processes() -> HashSet<u32> {
    unsafe extern "system" fn visit(window: HWND, lparam: LPARAM) -> BOOL {
        let pids = unsafe { &mut *(lparam.0 as *mut HashSet<u32>) };
        let own = unsafe {
            IsWindowVisible(window).as_bool()
                && !GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid())
                && GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 == 0
        };
        if own {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
            pids.insert(pid);
        }
        true.into()
    }
    let mut pids = HashSet::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut pids as *mut HashSet<u32> as isize));
    }
    pids
}

/// When `pid` was started, or `None` if it can't be opened.
fn creation_time(pid: u32) -> Option<u64> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let result = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        result.ok()?;
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEAM: u32 = 10;
    const GAME: u32 = 20;
    const CIDER: u32 = 30;
    const WEBVIEW: u32 = 40;
    const AUDIO: u32 = 50;

    /// A tree of `(pid, parent, has a window of its own)`, each process
    /// started after the ones listed before it.
    fn tree(processes: &[(u32, u32, bool)]) -> ProcessTree {
        ProcessTree {
            parents: processes.iter().map(|&(pid, parent, _)| (pid, parent)).collect(),
            names: HashMap::new(),
            started: RefCell::new(
                processes
                    .iter()
                    .enumerate()
                    .map(|(order, &(pid, ..))| (pid, Some(order as u64)))
                    .collect(),
            ),
            windowed: OnceCell::from(
                processes
                    .iter()
                    .filter(|&&(.., window)| window)
                    .map(|&(pid, ..)| pid)
                    .collect::<HashSet<u32>>(),
            ),
        }
    }

    #[test]
    fn a_game_steam_started_is_its_own_app_while_steam_plays_too() {
        // Steam, hidden in the tray, has a voice chat session.
        let tree = tree(&[(STEAM, 0, false), (GAME, STEAM, true)]);
        let apps = tree.owning_apps(&HashSet::from([STEAM, GAME]));
        assert_eq!(apps[&GAME], GAME);
        assert_eq!(apps[&STEAM], STEAM);
    }

    #[test]
    fn a_helper_without_a_window_is_its_apps() {
        // Cider plays from its WebView2 browser's audio service.
        let tree = tree(&[(CIDER, 0, true), (WEBVIEW, CIDER, false), (AUDIO, WEBVIEW, false)]);
        let apps = tree.owning_apps(&HashSet::from([CIDER, AUDIO]));
        assert_eq!(apps[&AUDIO], CIDER);
    }

    #[test]
    fn a_games_helper_is_the_games_not_the_launchers() {
        let tree = tree(&[(STEAM, 0, false), (GAME, STEAM, true), (AUDIO, GAME, false)]);
        let apps = tree.owning_apps(&HashSet::from([STEAM, GAME, AUDIO]));
        assert_eq!(apps[&AUDIO], GAME);
        assert_eq!(apps[&GAME], GAME);
    }

    #[test]
    fn nothing_above_a_program_of_its_own_counts_even_when_it_is_silent() {
        // The game's window is in between, though it has no session itself.
        let tree = tree(&[(STEAM, 0, false), (GAME, STEAM, true), (AUDIO, GAME, false)]);
        let apps = tree.owning_apps(&HashSet::from([STEAM, AUDIO]));
        assert_eq!(apps[&AUDIO], AUDIO);
    }

    #[test]
    fn a_reused_parent_id_is_not_the_parent() {
        // AUDIO's parent exited, and STEAM, started later, got its id.
        let tree = tree(&[(AUDIO, STEAM, false), (STEAM, 0, false)]);
        let apps = tree.owning_apps(&HashSet::from([STEAM, AUDIO]));
        assert_eq!(apps[&AUDIO], AUDIO);
    }

    #[test]
    fn routing_a_launcher_still_takes_the_games_it_started() {
        let tree = tree(&[(STEAM, 0, false), (GAME, STEAM, true)]);
        let owners = HashSet::from([STEAM, GAME]);
        assert_eq!(tree.app_audio_processes(STEAM, &owners), owners);
    }
}
