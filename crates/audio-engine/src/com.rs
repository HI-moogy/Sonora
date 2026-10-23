use windows::core::w;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW};

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

/// Puts the calling thread in Windows' "Pro Audio" scheduling class (MMCSS)
/// for the lifetime of this guard, so short audio buffers keep being filled
/// on time while the PC is busy (a game loading, say).
pub(crate) struct ProAudioThread(HANDLE);

impl ProAudioThread {
    /// `None` if Windows refuses (the thread just keeps normal priority).
    pub(crate) fn join() -> Option<Self> {
        let mut task_index = 0;
        unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }.ok().map(Self)
    }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.0);
        }
    }
}
