//! Playing to an output: a playback stream in the mixer's format (48 kHz
//! stereo f32), kept only as full as the mixer asks.

use libpulse_binding as pa;
use pa::def::BufferAttr;
use pa::sample::{Format, Spec};
use pa::stream::{FlagSet as StreamFlags, SeekMode, State as StreamState, Stream};

use super::conn::SAMPLE_RATE;
use super::Connection;
use crate::error::{Error, Result};

/// The most the server holds for Sonora; the mixer keeps far less queued.
const CAPACITY_MS: u32 = 200;
const BYTES_PER_FRAME: u32 = 8;

/// One opened output, kept alive only while something plays on it.
pub(crate) struct DeviceOutput {
    // Dropped before its connection.
    stream: Stream,
    connection: Connection,
    capacity_frames: u32,
    bytes: Vec<u8>,
}

impl DeviceOutput {
    /// Opens `device_id` (`None`: the default output). A chosen output that
    /// goes away fails the stream, so the mixer can fall back to the default
    /// or go silent, as the channel is set to; the default one follows the
    /// server's default.
    pub(crate) fn open(device_id: Option<&str>) -> Result<Self> {
        let mut connection = Connection::open("Sonora output")?;
        let spec = Spec { format: Format::F32le, channels: 2, rate: SAMPLE_RATE };
        let mut stream = Stream::new(&mut connection.context, "Sonora", &spec, None)
            .ok_or_else(|| Error::new("couldn't create a playback stream"))?;
        let capacity_frames = SAMPLE_RATE / 1000 * CAPACITY_MS;
        // No prebuffering: play from the first sample, and don't pause to
        // rebuffer after running dry (the mixer just catches up).
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: capacity_frames * BYTES_PER_FRAME,
            prebuf: 0,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let flags = if device_id.is_some() { StreamFlags::DONT_MOVE } else { StreamFlags::NOFLAGS };
        stream
            .connect_playback(device_id, Some(&attr), flags, None, None)
            .map_err(|error| Error::new(format!("couldn't play on {}: {error}", device_id.unwrap_or("the default output"))))?;
        connection.wait_ready(&stream)?;
        Ok(Self { stream, connection, capacity_frames, bytes: Vec::new() })
    }

    /// The most frames that can be queued.
    pub(crate) fn capacity_frames(&self) -> u32 {
        self.capacity_frames
    }

    /// Frames written but not played yet. An error means the output is
    /// gone (unplugged, or the server closed the stream).
    pub(crate) fn queued_frames(&mut self) -> Result<u32> {
        self.connection.iterate()?;
        if !matches!(self.stream.get_state(), StreamState::Ready) {
            return Err(Error::new("the output went away"));
        }
        // The server asks for what it has room for: the rest is queued.
        let room = self.stream.writable_size().unwrap_or(0) as u32 / BYTES_PER_FRAME;
        Ok(self.capacity_frames.saturating_sub(room))
    }

    /// Queues interleaved stereo `samples`; at most the room left
    /// ([`Self::capacity_frames`] minus [`Self::queued_frames`]).
    pub(crate) fn write(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.bytes.clear();
        self.bytes.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        self.stream
            .write(&self.bytes, None, 0, SeekMode::Relative)
            .map_err(|error| Error::new(format!("couldn't play: {error}")))
    }
}

impl Drop for DeviceOutput {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}
