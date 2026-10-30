//! Noticing that an app has closed. It's watched through a handle opened
//! while it ran: once it's gone, its pid can belong to another process.

use windows::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

/// A process watched for exiting.
pub(crate) struct ProcessWatch {
    handle: HANDLE,
    /// Whether the handle can be waited on. Some processes (protected
    /// games, say) only let it be opened to query, and the exit code tells.
    waitable: bool,
}

impl ProcessWatch {
    /// Starts watching `pid`; `None` if it can't be opened, and then
    /// whether it exits stays unknown.
    pub(crate) fn open(pid: u32) -> Option<Self> {
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                return Some(Self { handle, waitable: true });
            }
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            Some(Self { handle, waitable: false })
        }
    }

    /// Whether the process has exited.
    pub(crate) fn exited(&self) -> bool {
        unsafe {
            if self.waitable {
                return WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0;
            }
            // One that exits with the very code that means "running" (259)
            // reads as running.
            let mut code = 0;
            GetExitCodeProcess(self.handle, &mut code).is_ok() && code != STILL_ACTIVE.0 as u32
        }
    }
}

impl Drop for ProcessWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn notices_a_process_exit() {
        // cmd waits for its input until it's closed.
        let mut child = Command::new("cmd").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let watch = ProcessWatch::open(child.id()).unwrap();
        assert!(!watch.exited());
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(watch.exited());
    }
}
