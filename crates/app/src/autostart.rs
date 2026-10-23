//! Starting Sonora when you sign in to Windows: a value under the current
//! user's `Run` key that launches it hidden in the tray.

use windows::core::{w, Error, PCWSTR, Result};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Sonora");

/// Command-line flag for starting hidden in the tray, as at sign-in.
pub const BACKGROUND_ARG: &str = "--background";

/// The command that starts this sonora.exe in the background.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {BACKGROUND_ARG}", exe.display()))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Whether Sonora starts with Windows, from this location. A value left
/// by a copy elsewhere doesn't count, so turning it on points it here.
pub fn is_enabled() -> bool {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return false;
    }
    let len = (size as usize / 2).saturating_sub(1);
    command().is_some_and(|command| String::from_utf16_lossy(&buffer[..len]).eq_ignore_ascii_case(&command))
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    if enabled {
        let command = wide(&command().ok_or_else(Error::empty)?);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
            .ok()
        }
    } else {
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) } {
            error if error == ERROR_FILE_NOT_FOUND => Ok(()),
            result => result.ok(),
        }
    }
}

/// Removes the Start with Windows entry left by sonar-rs (Sonora's old
/// name), returning whether there was one, so it can be turned on again
/// under the new name.
pub fn take_legacy_entry() -> bool {
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("sonar-rs")) }.is_ok()
}
