//! Moving over from sonar-rs, Sonora's old name, on first start: the
//! settings folder, the default clips folder, and Start with Windows.
//! Nothing is deleted; a folder is only moved when the new one doesn't
//! exist yet, so a second run (or a failed move) changes nothing.

use std::path::Path;

use crate::autostart;

const OLD_NAME: &str = "sonar-rs";

/// Moves `old` to `new` if `old` exists and `new` doesn't. Returns a note
/// for the user if the move was needed but failed (e.g. a file is in use).
fn move_folder(old: &Path, new: &Path, what: &str) -> Option<String> {
    if new.exists() || !old.is_dir() {
        return None;
    }
    std::fs::rename(old, new).err().map(|error| {
        format!("Couldn't move your {what} from {} to {} ({error}); it'll be tried again next start.", old.display(), new.display())
    })
}

/// Runs before anything reads settings. Returns notes about anything that
/// couldn't be moved, to show in the window.
pub fn move_from_sonar_rs() -> Vec<String> {
    let mut notes = Vec::new();

    let config = common::config_dir();
    if let Some(parent) = config.parent() {
        notes.extend(move_folder(&parent.join(OLD_NAME), &config, "settings"));
    }

    // Clips only move if they were in the default folder; a folder you
    // picked yourself stays where it is.
    let clips = capture_engine::default_clips_dir();
    if common::load_presets().moments.clips_dir.is_none() {
        if let Some(parent) = clips.parent() {
            notes.extend(move_folder(&parent.join(OLD_NAME), &clips, "clips"));
        }
    }

    if autostart::take_legacy_entry() {
        let _ = autostart::set_enabled(true);
    }
    notes
}
