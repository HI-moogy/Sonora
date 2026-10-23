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
    fn this_process_is_its_own_family() {
        let me = std::process::id();
        assert!(with_descendants(me).contains(&me));
        assert!(exists(me));
        assert!(executable_name(me).is_some());
    }
}
