pub mod atomic_f32;
pub mod denoise;
pub mod eq;
pub mod error;
pub mod mixer;
pub mod surround;

#[cfg(windows)]
mod app_capture;
#[cfg(windows)]
pub mod app_output;
#[cfg(windows)]
pub mod com;
#[cfg(windows)]
pub mod devices;
#[cfg(windows)]
pub mod loopback;
#[cfg(windows)]
pub mod mic;
#[cfg(windows)]
mod output;
#[cfg(windows)]
pub mod passthrough;
#[cfg(windows)]
pub mod process_tree;
#[cfg(windows)]
mod process_watch;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod session_volume;
#[cfg(windows)]
pub mod sessions;

#[cfg(target_os = "linux")]
pub mod pulse;

#[cfg(windows)]
pub use com::ComGuard;
pub use denoise::{MicCleanup, MicTest};
pub use eq::{EQ_BAND_COUNT, EQ_BAND_FREQUENCIES, EQ_GAIN_RANGE_DB};
pub use error::{Error, Result};
pub use mixer::{BusInfo, BusTap, Mixer, SourceInfo};
#[cfg(windows)]
pub use passthrough::run_passthrough;
pub use platform::{
    enumerate_sessions, list_input_devices, list_output_devices, per_app_capture_supported, process_executable_name, AudioDevice,
    AudioSession,
};

/// What each platform provides the shared mixer: capturing an app and a
/// microphone, playing to a device, and what the mixer keeps set up while
/// it runs.
#[cfg(windows)]
mod platform {
    pub(crate) use crate::app_capture::run_capture_thread as run_app_capture;
    pub use crate::devices::{list_input_devices, list_output_devices, AudioDevice};
    pub use crate::loopback::per_app_capture_supported;
    pub(crate) use crate::mic::run_mic_capture_thread as run_mic_capture;
    pub(crate) use crate::output::DeviceOutput;
    pub use crate::sessions::{enumerate_sessions, process_executable_name, AudioSession};

    use crate::com::{ComGuard, ProAudioThread};
    use crate::error::Result;

    /// Nothing stays set up on Windows; starting gives back any app a
    /// previous run left turned down.
    pub(crate) struct MixerSetup;

    impl MixerSetup {
        pub(crate) fn start() -> Result<Self> {
            crate::session_volume::recover(&[]);
            Ok(Self)
        }
    }

    /// Sets up the calling thread for audio: COM, and Windows' Pro Audio
    /// scheduling, both for as long as the guard lives.
    pub(crate) fn audio_thread() -> Result<(ComGuard, Option<ProAudioThread>)> {
        Ok((ComGuard::init()?, ProAudioThread::join()))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    pub use crate::pulse::devices::{list_input_devices, list_output_devices, AudioDevice};
    pub(crate) use crate::pulse::capture::run_app_capture;
    pub(crate) use crate::pulse::mic::run_mic_capture;
    pub(crate) use crate::pulse::output::DeviceOutput;
    pub use crate::pulse::sessions::{enumerate_sessions, process_executable_name, AudioSession};
    pub(crate) use crate::pulse::setup::MixerSetup;

    use crate::error::Result;

    /// The sound server can always record one app (its own sink).
    pub fn per_app_capture_supported() -> bool {
        true
    }

    /// Nothing to set up per thread: the sound server keeps time.
    pub(crate) fn audio_thread() -> Result<()> {
        Ok(())
    }
}
