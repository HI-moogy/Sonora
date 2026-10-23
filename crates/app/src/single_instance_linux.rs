//! Only one Sonora at a time on Linux: two would move the same apps'
//! streams back and forth. A second launch asks the first to show its
//! window instead, through a socket next to the lock file.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use eframe::egui;

/// The main window's title.
pub const WINDOW_TITLE: &str = "Sonora";

/// Held for the process lifetime.
pub struct InstanceLock {
    _file: File,
}

/// Where the lock and socket live: the user's runtime folder (cleared at
/// logout), or a per-user folder in /tmp.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|dir| dir.is_dir()).unwrap_or_else(|| {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        std::env::temp_dir().join(format!("sonora-{user}"))
    })
}

fn socket_path() -> PathBuf {
    runtime_dir().join("sonora.sock")
}

/// Returns the lock if this is the only running instance; otherwise asks
/// the running one to show its window and returns `None`.
pub fn acquire() -> Option<InstanceLock> {
    let dir = runtime_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = File::options().create(true).truncate(false).write(true).open(dir.join("sonora.lock")) else {
        // Can't tell: run rather than not at all.
        return Some(InstanceLock { _file: File::open("/dev/null").ok()? });
    };
    match file.try_lock() {
        Ok(()) => Some(InstanceLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => {
            if let Ok(mut running) = UnixStream::connect(socket_path()) {
                let _ = running.write_all(b"show");
            }
            None
        }
        Err(std::fs::TryLockError::Error(_)) => Some(InstanceLock { _file: file }),
    }
}

/// Shows the window whenever another launch asks.
pub fn listen(ctx: egui::Context) {
    let path = socket_path();
    // A socket file outlives a crash; the lock says it's ours now.
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else { return };
    let _ = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4];
            if stream.read_exact(&mut request).is_ok() && &request == b"show" {
                crate::tray::show_main_window(&ctx);
            }
        }
    });
}
