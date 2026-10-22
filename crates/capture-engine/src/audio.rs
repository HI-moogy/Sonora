//! Clip audio: per-track PCM (the mixer's buses, plus desktop audio), cut
//! into time-aligned blocks, volume-scaled, and AAC-encoded into rolling
//! buffers alongside the video.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::aac::AacEncoder;
use crate::com::MfThread;

pub(crate) const SAMPLE_RATE: u32 = 48_000;
pub(crate) const CHANNELS: usize = 2;
/// One AAC frame. Every track is cut on the same block boundaries, so the
/// tracks stay sample-aligned.
pub(crate) const BLOCK_FRAMES: i64 = 1024;
/// How long a block waits for its audio to arrive before it's encoded; any
/// still missing then becomes silence.
const LATENCY_FRAMES: i64 = SAMPLE_RATE as i64 / 10;
/// A chunk whose arrival is this far from where the previous one ended
/// re-anchors the track to the clock instead of continuing seamlessly.
const RESYNC_FRAMES: i64 = SAMPLE_RATE as i64 / 20;
/// Pushed audio older than this is discarded unread (encoder stalled).
const MAX_BACKLOG_FRAMES: i64 = 2 * SAMPLE_RATE as i64;
const HNS_PER_SECOND: i64 = 10_000_000;

/// Everything playing on the default output device. Always a clip's last
/// audio track, and the one players play by default (most only ever play
/// one audio track).
pub const DESKTOP_TRACK: &str = "Desktop";

struct Chunk {
    /// Time of the first frame, in 48 kHz frames since the recorder started.
    start: i64,
    samples: Vec<f32>,
}

impl Chunk {
    fn end(&self) -> i64 {
        self.start + (self.samples.len() / CHANNELS) as i64
    }
}

#[derive(Default)]
struct Fifo {
    chunks: VecDeque<Chunk>,
    next_start: Option<i64>,
}

impl Fifo {
    /// Places a chunk that arrived (finished rendering) at frame time
    /// `arrived`. Consecutive chunks are laid end to end by sample count so
    /// scheduling jitter doesn't add clicks; a big jump means a real gap.
    fn push(&mut self, samples: &[f32], arrived: i64) {
        let frames = (samples.len() / CHANNELS) as i64;
        let by_clock = arrived - frames;
        let start = match self.next_start {
            Some(next) if (by_clock - next).abs() <= RESYNC_FRAMES => next,
            _ => by_clock,
        };
        self.next_start = Some(start + frames);
        self.chunks.push_back(Chunk { start, samples: samples.to_vec() });
        while self.chunks.front().is_some_and(|c| c.end() < arrived - MAX_BACKLOG_FRAMES) {
            self.chunks.pop_front();
        }
    }

    /// Copies whatever audio covers [start, start + BLOCK_FRAMES) into `out`
    /// (silence elsewhere) and drops chunks that end within the block.
    fn take_block(&mut self, start: i64, out: &mut [f32]) {
        let end = start + BLOCK_FRAMES;
        out.fill(0.0);
        for chunk in &self.chunks {
            let from = start.max(chunk.start);
            let to = end.min(chunk.end());
            if from >= to {
                continue;
            }
            let src = ((from - chunk.start) as usize) * CHANNELS;
            let dst = ((from - start) as usize) * CHANNELS;
            let len = ((to - from) as usize) * CHANNELS;
            out[dst..dst + len].copy_from_slice(&chunk.samples[src..src + len]);
        }
        while self.chunks.front().is_some_and(|c| c.end() <= end) {
            self.chunks.pop_front();
        }
    }
}

struct Track {
    name: String,
    fifo: Mutex<Fifo>,
    /// Clip volume (f32 bits); doesn't affect what you hear.
    volume: AtomicU32,
}

pub(crate) struct AudioShared {
    epoch: Instant,
    tracks: Vec<Track>,
    running: AtomicBool,
}

impl AudioShared {
    pub(crate) fn new(epoch: Instant, names: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            epoch,
            tracks: names
                .iter()
                .map(|name| Track { name: name.to_string(), fifo: Mutex::default(), volume: AtomicU32::new(1.0f32.to_bits()) })
                .collect(),
            running: AtomicBool::new(true),
        })
    }

    fn now_frames(&self) -> i64 {
        (self.epoch.elapsed().as_nanos() * SAMPLE_RATE as u128 / 1_000_000_000) as i64
    }

    pub(crate) fn set_volume(&self, track: &str, volume: f32) {
        if let Some(track) = self.tracks.iter().find(|t| t.name == track) {
            track.volume.store(volume.max(0.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// A cheap, cloneable handle the mixer's render thread uses to feed the
/// recorder. Pushes after the recorder stops are ignored.
#[derive(Clone)]
pub struct AudioInput(pub(crate) Arc<AudioShared>);

impl AudioInput {
    /// `samples` is interleaved stereo f32 at 48 kHz that just finished
    /// rendering. Unknown track names are ignored.
    pub fn push(&self, track: &str, samples: &[f32]) {
        let shared = &self.0;
        if !shared.running.load(Ordering::Relaxed) {
            return;
        }
        if let Some(t) = shared.tracks.iter().find(|t| t.name == track) {
            t.fifo.lock().unwrap().push(samples, shared.now_frames());
        }
    }
}

/// One encoded AAC frame.
#[derive(Clone)]
pub(crate) struct AudioFrame {
    pub pts: i64,
    pub data: Bytes,
}

/// The most recent `span` of one encoded track.
pub(crate) struct AudioRing {
    frames: VecDeque<AudioFrame>,
    span: i64,
    bytes: usize,
}

impl AudioRing {
    fn push(&mut self, frame: AudioFrame) {
        let cutoff = frame.pts - self.span;
        self.bytes += frame.data.len();
        self.frames.push_back(frame);
        while self.frames.front().is_some_and(|f| f.pts < cutoff) {
            if let Some(dropped) = self.frames.pop_front() {
                self.bytes -= dropped.data.len();
            }
        }
    }

    /// Frames from the one nearest `from_pts` onward, up to `to_pts`.
    pub(crate) fn snapshot(&self, from_pts: i64, to_pts: i64) -> Vec<AudioFrame> {
        let half_block = BLOCK_FRAMES * HNS_PER_SECOND / SAMPLE_RATE as i64 / 2;
        self.frames
            .iter()
            .filter(|f| f.pts >= from_pts - half_block && f.pts < to_pts)
            .cloned()
            .collect()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Encoded rings for each track, in track order.
pub(crate) type AudioRings = Arc<Mutex<Vec<AudioRing>>>;

pub(crate) fn new_rings(track_count: usize, span: i64) -> AudioRings {
    // A second of slack so the audio always reaches back as far as the
    // keyframe-aligned video does.
    let span = span + HNS_PER_SECOND;
    let rings = (0..track_count).map(|_| AudioRing { frames: VecDeque::new(), span, bytes: 0 }).collect();
    Arc::new(Mutex::new(rings))
}

/// Runs on the "moments-audio" thread until `shared` is stopped.
pub(crate) fn run(shared: Arc<AudioShared>, rings: AudioRings) -> windows::core::Result<()> {
    let _mf = MfThread::init()?;
    let mut encoders: Vec<AacEncoder> = shared.tracks.iter().map(|_| AacEncoder::new()).collect::<Result<_, _>>()?;
    let mut block = vec![0.0f32; BLOCK_FRAMES as usize * CHANNELS];
    let mut pcm = vec![0i16; block.len()];
    let mut block_start = shared.now_frames();

    while shared.running.load(Ordering::Relaxed) {
        while block_start + BLOCK_FRAMES + LATENCY_FRAMES <= shared.now_frames() {
            let pts = block_start * HNS_PER_SECOND / SAMPLE_RATE as i64;
            for (index, track) in shared.tracks.iter().enumerate() {
                track.fifo.lock().unwrap().take_block(block_start, &mut block);
                let volume = f32::from_bits(track.volume.load(Ordering::Relaxed));
                block.iter_mut().for_each(|sample| *sample *= volume);
                encode_into(&mut encoders[index], &block, &mut pcm, pts, &rings, index)?;
            }
            block_start += BLOCK_FRAMES;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn encode_into(
    encoder: &mut AacEncoder,
    samples: &[f32],
    pcm: &mut [i16],
    pts: i64,
    rings: &AudioRings,
    ring: usize,
) -> windows::core::Result<()> {
    for (out, &sample) in pcm.iter_mut().zip(samples) {
        *out = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
    }
    encoder.encode(pcm, pts, &mut |pts, data| rings.lock().unwrap()[ring].push(AudioFrame { pts, data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * CHANNELS]
    }

    #[test]
    fn jittery_chunks_are_laid_end_to_end() {
        let mut fifo = Fifo::default();
        fifo.push(&stereo(480, 1.0), 10_480); // starts at 10_000
        fifo.push(&stereo(480, 2.0), 10_970); // 10 frames "late": still continuous
        assert_eq!(fifo.chunks[1].start, 10_480);
    }

    #[test]
    fn a_real_gap_reanchors_to_the_clock() {
        let mut fifo = Fifo::default();
        fifo.push(&stereo(480, 1.0), 10_480);
        fifo.push(&stereo(480, 2.0), 20_000);
        assert_eq!(fifo.chunks[1].start, 19_520);
    }

    #[test]
    fn block_takes_overlapping_audio_and_pads_silence() {
        let mut fifo = Fifo::default();
        fifo.push(&stereo(512, 0.5), 1_512); // frames 1000..1512
        let mut out = vec![9.0; BLOCK_FRAMES as usize * CHANNELS];
        fifo.take_block(1_000, &mut out); // block 1000..2024
        assert!(out[..512 * CHANNELS].iter().all(|&s| s == 0.5));
        assert!(out[512 * CHANNELS..].iter().all(|&s| s == 0.0));
        assert!(fifo.chunks.is_empty(), "fully consumed chunk is dropped");
    }

    #[test]
    fn chunk_spanning_two_blocks_is_split_between_them() {
        let mut fifo = Fifo::default();
        fifo.push(&stereo(600, 0.25), 1_600); // frames 1000..1600
        let mut out = vec![0.0; BLOCK_FRAMES as usize * CHANNELS];
        fifo.take_block(0, &mut out); // block 0..1024 gets frames 1000..1024
        assert!(out[..1000 * CHANNELS].iter().all(|&s| s == 0.0));
        assert!(out[1000 * CHANNELS..].iter().all(|&s| s == 0.25));
        assert_eq!(fifo.chunks.len(), 1, "partly used chunk is kept");
        fifo.take_block(1_024, &mut out); // gets 1024..1600
        assert!(out[..576 * CHANNELS].iter().all(|&s| s == 0.25));
        assert!(out[576 * CHANNELS..].iter().all(|&s| s == 0.0));
    }
}
