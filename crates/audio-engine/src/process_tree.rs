//! Which processes make up an app's audio.
//!
//! Apps often play audio from a helper process rather than themselves:
//! WebView2 and Chromium-based apps play from a browser audio-service child.
//! Measured with Cider (a WebView2 app): Windows' per-app capture of Cider
//! itself, which is meant to include its child processes, returned silence,
//! while capturing its WebView2 browser process returned the music. So an
//! app's audio is followed into the descendants that own audio sessions.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Running processes and the parent each was started by.
pub struct ProcessTree {
    parents: HashMap<u32, u32>,
    started: RefCell<HashMap<u32, Option<u64>>>,
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

/// The executable names (lowercase, e.g. "cider.exe") of every running
/// process.
pub fn running_executables() -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    visit_processes(|entry| {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        names.insert(String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase());
    })?;
    Ok(names)
}

impl ProcessTree {
    pub fn snapshot() -> Result<Self> {
        let mut parents = HashMap::new();
        visit_processes(|entry| {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        })?;
        Ok(Self {
            parents,
            started: RefCell::default(),
        })
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

    /// The app each audio-session owner in `owners` belongs to: its
    /// furthest ancestor that also owns a session, or itself.
    pub fn owning_apps(&self, owners: &HashSet<u32>) -> HashMap<u32, u32> {
        owners
            .iter()
            .map(|&pid| {
                let app = self
                    .ancestors(pid)
                    .filter(|p| owners.contains(p))
                    .last()
                    .unwrap_or(pid);
                (pid, app)
            })
            .collect()
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
