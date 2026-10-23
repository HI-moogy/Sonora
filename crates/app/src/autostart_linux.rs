//! Starting Sonora when you log in on Linux: a desktop entry in
//! `~/.config/autostart` that launches it hidden in the tray.

use std::path::PathBuf;

/// Command-line flag for starting hidden in the tray, as at login.
pub const BACKGROUND_ARG: &str = "--background";

fn entry_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("autostart").join("sonora.desktop"))
}

/// The program to start: the AppImage itself when running from one (its
/// contents are mounted somewhere new each run), else this executable.
fn program() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok())
}

/// `path` quoted as the desktop entry spec wants in an `Exec` line.
fn exec_quoted(path: &std::path::Path) -> String {
    let escaped: String = path
        .display()
        .to_string()
        .chars()
        .flat_map(|c| match c {
            '"' | '`' | '$' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

fn entry(program: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Sonora\nComment=Per-app audio mixer\nExec={} {BACKGROUND_ARG}\nIcon=sonora\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec_quoted(program)
    )
}

/// Whether Sonora starts at login, from this location. An entry left by a
/// copy elsewhere doesn't count, so turning it on points it here.
pub fn is_enabled() -> bool {
    let (Some(path), Some(program)) = (entry_path(), program()) else { return false };
    std::fs::read_to_string(path).is_ok_and(|text| text == entry(&program))
}

pub fn set_enabled(enabled: bool) -> std::io::Result<()> {
    let path = entry_path().ok_or_else(|| std::io::Error::other("no home folder"))?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let program = program().ok_or_else(|| std::io::Error::other("couldn't find Sonora's own program"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, entry(&program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_this_program_in_the_background() {
        let text = entry(std::path::Path::new("/home/me/Apps/Sonora $1.AppImage"));
        assert!(text.contains("Exec=\"/home/me/Apps/Sonora \\$1.AppImage\" --background\n"));
        assert!(text.starts_with("[Desktop Entry]\n"));
    }
}
