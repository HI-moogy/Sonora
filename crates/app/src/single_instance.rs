use std::path::PathBuf;

use windows::core::{w, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId};

/// The main window's title; also how a second launch finds the first window.
pub const WINDOW_TITLE: &str = "Sonora";

/// Held for the process lifetime. Two mixers would capture and mute the same
/// app sessions and fight over them, so only one instance may run.
pub struct InstanceLock(HANDLE);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Returns the lock if this is the only running instance; otherwise brings
/// the existing instance's window to the front and returns `None`.
pub fn acquire() -> Option<InstanceLock> {
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\sonora-single-instance")) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        focus_existing_window();
        return None;
    }
    Some(InstanceLock(handle))
}

struct Search {
    own_exe: PathBuf,
    found: Option<HWND>,
}

fn focus_existing_window() {
    let Ok(own_exe) = std::env::current_exe() else {
        return;
    };
    let mut search = Search { own_exe, found: None };
    unsafe {
        let _ = EnumWindows(Some(match_window), LPARAM(&mut search as *mut Search as isize));
        // It may be hidden in the tray, not just minimized.
        if let Some(hwnd) = search.found {
            crate::tray::show_window(hwnd);
        }
    }
}

unsafe extern "system" fn match_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam.0 as *mut Search);

    let mut title = [0u16; 64];
    let len = GetWindowTextW(hwnd, &mut title) as usize;
    if String::from_utf16_lossy(&title[..len]) != WINDOW_TITLE {
        return true.into();
    }

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == std::process::id() || process_image(pid).as_ref() != Some(&search.own_exe) {
        return true.into();
    }

    search.found = Some(hwnd);
    false.into()
}

fn process_image(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}
