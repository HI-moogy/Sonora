//! Processes, read from /proc.

use std::collections::{HashMap, HashSet};

/// `pid` and every process started under it: apps like browsers and
/// Discord play audio from a helper process.
pub(crate) fn with_descendants(pid: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(child) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
            if let Some(parent) = std::fs::read_to_string(format!("/proc/{child}/stat")).ok().and_then(|stat| parent_of(&stat)) {
                children.entry(parent).or_default().push(child);
            }
        }
    }
    let mut found = HashSet::from([pid]);
    let mut next = vec![pid];
    while let Some(parent) = next.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if found.insert(child) {
                next.push(child);
            }
        }
    }
    found
}

/// The app `pid` is part of: its furthest ancestor running the same
/// program. Browsers and Electron apps (Discord, Spotify) play audio from a
/// helper process started from their own program; this finds the app.
pub(crate) fn app_root(pid: u32) -> u32 {
    root_with(pid, parent_pid, executable_name)
}

fn root_with(pid: u32, parent: impl Fn(u32) -> Option<u32>, program: impl Fn(u32) -> Option<String>) -> u32 {
    let Some(name) = program(pid) else { return pid };
    let mut root = pid;
    // Bounded, in case of a pid cycle while processes come and go.
    for _ in 0..64 {
        match parent(root).filter(|&up| up > 1 && up != root) {
            Some(up) if program(up).as_ref() == Some(&name) => root = up,
            _ => break,
        }
    }
    root
}

fn parent_pid(pid: u32) -> Option<u32> {
    parent_of(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// The parent pid in a `/proc/<pid>/stat` line. The name before it is in
/// parentheses and may itself contain spaces and parentheses.
fn parent_of(stat: &str) -> Option<u32> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}

pub(crate) fn exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// The program's file name ("firefox", "Discord"), or its short name if
/// its executable can't be read.
pub(crate) fn executable_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|name| name.trim().to_string()))
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_is_found_after_any_name() {
        assert_eq!(parent_of("1234 (firefox) S 1000 1234 1234 0"), Some(1000));
        assert_eq!(parent_of("1234 (Web Content (1)) S 999 1234"), Some(999));
        assert_eq!(parent_of("garbage"), None);
    }

    #[test]
    fn a_helper_belongs_to_the_furthest_ancestor_running_its_program() {
        // 1 init > 10 shell > 20 discord > 30 discord helper > 40 discord helper
        let parent = |pid| [(40, 30), (30, 20), (20, 10), (10, 1)].into_iter().find(|&(c, _)| c == pid).map(|(_, p)| p);
        let program = |pid| Some(if pid >= 20 { "Discord" } else { "bash" }.to_string());
        assert_eq!(root_with(40, parent, program), 20);
        assert_eq!(root_with(20, parent, program), 20);
        assert_eq!(root_with(10, parent, program), 10);
        // A process whose program can't be read is its own app.
        assert_eq!(root_with(40, parent, |_| None), 40);
    }

    #[test]
    fn this_process_is_its_own_family() {
        let me = std::process::id();
        assert!(with_descendants(me).contains(&me));
        assert!(exists(me));
        assert!(executable_name(me).is_some());
    }
}
