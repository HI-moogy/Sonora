//! The audio engine's errors. On Windows they're the system's own errors
//! (with their messages); elsewhere, a message saying what failed.

#[cfg(windows)]
pub use windows::core::{Error, Result};

#[cfg(windows)]
pub(crate) fn invalid_argument() -> Error {
    Error::from(windows::Win32::Foundation::E_INVALIDARG)
}

#[cfg(windows)]
pub(crate) fn timed_out() -> Error {
    Error::from(windows::Win32::Foundation::E_UNEXPECTED)
}

#[cfg(not(windows))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

#[cfg(not(windows))]
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(not(windows))]
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// What went wrong, for showing.
    pub fn message(&self) -> String {
        self.0.clone()
    }
}

#[cfg(not(windows))]
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(not(windows))]
impl std::error::Error for Error {}

#[cfg(not(windows))]
pub(crate) fn invalid_argument() -> Error {
    Error::new("not something Sonora can capture")
}

#[cfg(not(windows))]
pub(crate) fn timed_out() -> Error {
    Error::new("the sound server didn't answer in time")
}
