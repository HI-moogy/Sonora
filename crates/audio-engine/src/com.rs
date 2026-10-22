use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Initializes COM for the current thread for the lifetime of this guard.
pub struct ComGuard;

impl ComGuard {
    pub fn init() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(ComGuard)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
