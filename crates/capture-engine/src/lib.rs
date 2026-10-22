//! Moments: always-on recording of the primary monitor into a rolling
//! buffer, saved to MP4 on demand.

mod aac;
mod audio;
mod com;
mod convert;
mod d3d;
mod desktop;
mod duplication;
mod encoder;
mod h264;
mod mux;
mod recorder;
mod ring;

use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

pub use audio::{AudioInput, DESKTOP_TRACK};
pub use recorder::{ClipSaver, Recorder, RecorderStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    P720,
    P1080,
    P1440,
    P2160,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [Self::P720, Self::P1080, Self::P1440, Self::P2160];

    /// Encoded frame size. The desktop is scaled to fit, letterboxed if its
    /// aspect ratio isn't 16:9.
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::P2160 => (3840, 2160),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "4K",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSettings {
    pub resolution: Resolution,
    pub fps: u32,
    /// How far back a saved clip reaches.
    pub buffer_secs: u32,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self { resolution: Resolution::P1080, fps: 60, buffer_secs: 30 }
    }
}

impl VideoSettings {
    /// Constant bitrate for the encoder, in bits per second. The buffer holds
    /// `bitrate / 8 * buffer_secs` bytes, so this sets its memory use too.
    pub fn bitrate(&self) -> u32 {
        let base = match self.resolution {
            Resolution::P720 => 4_000_000,
            Resolution::P1080 => 8_000_000,
            Resolution::P1440 => 14_000_000,
            Resolution::P2160 => 30_000_000,
        };
        if self.fps > 30 { base * 3 / 2 } else { base }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("couldn't find the GPU output for the primary monitor")]
    NoPrimaryOutput,
    #[error("no hardware H.264 encoder is available")]
    NoHardwareEncoder,
    #[error("recorder thread exited during setup")]
    ThreadExited,
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("nothing has been recorded yet")]
    Empty,
    #[error("the recorder isn't running")]
    Stopped,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Mp4(#[from] mp4::Error),
}

/// A saved clip's length, read from its MP4 header (cheap: the media data
/// isn't read). `None` if the file isn't a readable MP4.
pub fn clip_duration(path: &std::path::Path) -> Option<std::time::Duration> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).ok()?;
    Some(reader.duration())
}

/// `Videos\sonar-rs`, the default place clips are saved.
pub fn default_clips_dir() -> PathBuf {
    let videos = unsafe { SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) }
        .ok()
        .map(|path| {
            let owned = unsafe { path.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(path.0 as *const _)) };
            PathBuf::from(owned)
        })
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Videos"));
    videos.join("sonar-rs")
}
