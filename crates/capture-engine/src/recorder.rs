use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::audio::{self, AudioInput, AudioRings, AudioShared};
use crate::com::MfThread;
use crate::convert::Converter;
use crate::d3d::open_primary_output;
use crate::duplication::{Desktop, Poll};
use crate::encoder::{EncoderConfig, H264Encoder};
use crate::h264::to_access_unit;
use crate::mux::{write_clip, Clip};
use crate::ring::{VideoRing, VideoSample};
use crate::{SaveError, StartError, VideoSettings};

const HNS_PER_SECOND: i64 = 10_000_000;
/// Comfortably more than the audio pipeline's lag (block wait + one block).
const AUDIO_CATCH_UP: Duration = Duration::from_millis(250);

enum Command {
    Save { dir: PathBuf, reply: Sender<Result<PathBuf, SaveError>> },
    Stop,
}

/// A cheap, cloneable handle for saving clips from any thread (e.g. a
/// global hotkey listener). Saves after the recorder stops fail with
/// [`SaveError::Stopped`].
#[derive(Clone)]
pub struct ClipSaver(Sender<Command>);

impl ClipSaver {
    /// Saves the buffered video and audio to a new MP4 in `dir`. The file is
    /// written on a background thread; the receiver yields its path once
    /// complete.
    pub fn save(&self, dir: impl Into<PathBuf>) -> Receiver<Result<PathBuf, SaveError>> {
        let (reply, result) = mpsc::channel();
        if self.0.send(Command::Save { dir: dir.into(), reply: reply.clone() }).is_err() {
            let _ = reply.send(Err(SaveError::Stopped));
        }
        result
    }
}

/// Live numbers for the UI.
#[derive(Default)]
pub struct RecorderStatus {
    buffered_ms: AtomicU64,
    buffered_bytes: AtomicUsize,
    dropped_frames: AtomicU64,
    error: Mutex<Option<String>>,
}

impl RecorderStatus {
    pub fn buffered(&self) -> Duration {
        Duration::from_millis(self.buffered_ms.load(Ordering::Relaxed))
    }
    pub fn buffered_bytes(&self) -> usize {
        self.buffered_bytes.load(Ordering::Relaxed)
    }
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }
    /// Set if the recorder stopped on its own because something failed.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

/// Continuously records the primary monitor (hardware-encoded H.264) and the
/// audio tracks fed through [`Recorder::audio_input`] into rolling in-memory
/// buffers. Dropping it stops recording.
pub struct Recorder {
    commands: Sender<Command>,
    worker: Option<JoinHandle<()>>,
    audio: Arc<AudioShared>,
    audio_worker: Option<JoinHandle<()>>,
    status: Arc<RecorderStatus>,
}

impl Recorder {
    /// `tracks` names the audio tracks clips get after the mix track, in
    /// order (for sonar-rs: the buses).
    pub fn start(settings: VideoSettings, tracks: &[&str]) -> Result<Self, StartError> {
        let epoch = Instant::now();
        let span = settings.buffer_secs as i64 * HNS_PER_SECOND;
        let status = Arc::new(RecorderStatus::default());
        let audio = AudioShared::new(epoch, tracks);
        let rings = audio::new_rings(tracks.len(), span);

        let audio_worker = {
            let (audio, rings, status) = (audio.clone(), rings.clone(), status.clone());
            std::thread::Builder::new()
                .name("moments-audio".into())
                .spawn(move || {
                    if let Err(error) = audio::run(audio, rings) {
                        *status.error.lock().unwrap() = Some(format!("audio: {error}"));
                    }
                })
                .expect("spawn moments audio thread")
        };

        let (commands, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_status = status.clone();
        let worker = std::thread::Builder::new()
            .name("moments-video".into())
            .spawn(move || run(settings, epoch, rings, command_rx, ready_tx, worker_status))
            .expect("spawn moments video thread");
        let recorder = Self {
            commands,
            worker: Some(worker),
            audio,
            audio_worker: Some(audio_worker),
            status,
        };
        // On failure, dropping `recorder` stops and joins both threads.
        ready_rx.recv().map_err(|_| StartError::ThreadExited)??;
        Ok(recorder)
    }

    /// Handle for feeding audio tracks (cheap to clone, safe to call from a
    /// real-time thread).
    pub fn audio_input(&self) -> AudioInput {
        AudioInput(self.audio.clone())
    }

    /// Scales one track in saved clips (and its share of the mix track).
    pub fn set_track_volume(&self, track: &str, volume: f32) {
        self.audio.set_volume(track, volume);
    }

    pub fn status(&self) -> &RecorderStatus {
        &self.status
    }

    pub fn saver(&self) -> ClipSaver {
        ClipSaver(self.commands.clone())
    }

    /// See [`ClipSaver::save`].
    pub fn save_clip(&self, dir: impl Into<PathBuf>) -> Receiver<Result<PathBuf, SaveError>> {
        self.saver().save(dir)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // An explicit stop: ClipSaver clones may keep the channel open.
        let _ = self.commands.send(Command::Stop);
        self.audio.stop();
        for worker in [self.worker.take(), self.audio_worker.take()].into_iter().flatten() {
            let _ = worker.join();
        }
    }
}

fn run(
    settings: VideoSettings,
    epoch: Instant,
    rings: AudioRings,
    commands: Receiver<Command>,
    ready: Sender<Result<(), StartError>>,
    status: Arc<RecorderStatus>,
) {
    let _mf = match MfThread::init() {
        Ok(mf) => mf,
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return;
        }
    };
    let mut session = match Session::open(&settings, epoch, rings) {
        Ok(session) => {
            let _ = ready.send(Ok(()));
            session
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = session.record(&settings, &commands, &status) {
        *status.error.lock().unwrap() = Some(error.to_string());
    }
}

/// Everything the recording loop owns.
struct Session {
    desktop: Desktop,
    converter: Converter,
    encoder: H264Encoder,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ring: VideoRing,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    epoch: Instant,
    audio: AudioRings,
}

impl Session {
    fn open(settings: &VideoSettings, epoch: Instant, audio: AudioRings) -> Result<Self, StartError> {
        let primary = open_primary_output()?;
        let desktop = Desktop::new(primary.device.clone(), primary.context.clone(), primary.output)?;
        let (width, height) = settings.resolution.size();
        let converter = Converter::new(
            &primary.device,
            &primary.context,
            &desktop.latest,
            (desktop.width, desktop.height),
            (width, height),
            settings.fps,
        )?;
        let encoder = H264Encoder::new(
            &primary.device,
            &EncoderConfig { width, height, fps: settings.fps, bitrate: settings.bitrate() },
        )?;
        Ok(Self {
            desktop,
            converter,
            encoder,
            device: primary.device,
            context: primary.context,
            ring: VideoRing::new(settings.buffer_secs as i64 * HNS_PER_SECOND),
            sps: None,
            pps: None,
            epoch,
            audio,
        })
    }

    fn record(
        &mut self,
        settings: &VideoSettings,
        commands: &Receiver<Command>,
        status: &RecorderStatus,
    ) -> windows::core::Result<()> {
        let interval = Duration::from_secs(1) / settings.fps;
        let epoch = self.epoch;
        let mut next_due = Instant::now();

        loop {
            match commands.try_recv() {
                Ok(Command::Save { dir, reply }) => self.save(settings, dir, reply),
                Err(TryRecvError::Empty) => {}
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return Ok(()),
            }

            // Sleep until the next frame is due, then take only the latest
            // desktop image. Duplication accumulates updates, so this copies
            // once per encoded frame instead of on every monitor refresh
            // (170 Hz on a high-refresh display, for a 60 fps recording).
            let wait = next_due.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
            if self.desktop.poll(Duration::ZERO)? == Poll::Resized {
                self.converter = Converter::new(
                    &self.device,
                    &self.context,
                    &self.desktop.latest,
                    (self.desktop.width, self.desktop.height),
                    settings.resolution.size(),
                    settings.fps,
                )?;
            }

            self.collect_encoded()?;

            let now = Instant::now();
            if now >= next_due {
                let pts = (now - epoch).as_nanos() as i64 / 100;
                // Only spend GPU time converting when the encoder can take
                // the frame.
                if self.encoder.wants_input() {
                    let frame = self.converter.convert()?;
                    self.encoder.try_encode(&frame, pts)?;
                } else {
                    status.dropped_frames.fetch_add(1, Ordering::Relaxed);
                }
                next_due += interval;
                if now > next_due + interval {
                    // Fell behind (stall, mode switch): resync instead of
                    // bursting to catch up.
                    next_due = now + interval;
                }
            }

            status.buffered_ms.store((self.ring.duration() / 10_000) as u64, Ordering::Relaxed);
            let audio_bytes: usize = self.audio.lock().unwrap().iter().map(|ring| ring.bytes()).sum();
            status.buffered_bytes.store(self.ring.bytes() + audio_bytes, Ordering::Relaxed);
        }
    }

    fn collect_encoded(&mut self) -> windows::core::Result<()> {
        let ring = &mut self.ring;
        let sps = &mut self.sps;
        let pps = &mut self.pps;
        self.encoder.pump(&mut |frame| {
            let unit = to_access_unit(&frame.annex_b);
            if unit.sps.is_some() {
                *sps = unit.sps;
            }
            if unit.pps.is_some() {
                *pps = unit.pps;
            }
            ring.push(VideoSample { pts: frame.pts, keyframe: unit.keyframe, data: unit.avcc.into() });
        })
    }

    fn save(&self, settings: &VideoSettings, dir: PathBuf, reply: Sender<Result<PathBuf, SaveError>>) {
        let (Some(sps), Some(pps)) = (self.sps.clone(), self.pps.clone()) else {
            let _ = reply.send(Err(SaveError::Empty));
            return;
        };
        let frames = self.ring.snapshot(settings.buffer_secs as i64 * HNS_PER_SECOND);
        let frame_duration = HNS_PER_SECOND / settings.fps as i64;
        let (width, height) = settings.resolution.size();
        let mut clip = Clip {
            width: width as u16,
            height: height as u16,
            sps,
            pps,
            frames,
            frame_duration,
            tracks: Vec::new(),
            mix: Vec::new(),
        };
        let audio = self.audio.clone();
        std::thread::Builder::new()
            .name("moments-save".into())
            .spawn(move || {
                // Audio is encoded a little behind real time (it waits for
                // late pushes), so give it time to reach the video's end.
                std::thread::sleep(AUDIO_CATCH_UP);
                if let (Some(first), Some(last)) = (clip.frames.first(), clip.frames.last()) {
                    let end = last.pts + clip.frame_duration;
                    let rings = audio.lock().unwrap();
                    clip.mix = rings[0].snapshot(first.pts, end);
                    clip.tracks = rings[1..].iter().map(|ring| ring.snapshot(first.pts, end)).collect();
                }
                let _ = reply.send(save_to(&dir, &clip));
            })
            .expect("spawn clip save thread");
    }
}

/// Writes under a temporary name first so a half-written file never shows
/// up as a clip.
fn save_to(dir: &Path, clip: &Clip) -> Result<PathBuf, SaveError> {
    std::fs::create_dir_all(dir)?;
    let path = unique_clip_path(dir);
    let partial = path.with_extension("mp4.partial");
    if let Err(error) = write_clip(&partial, clip) {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

fn unique_clip_path(dir: &Path) -> PathBuf {
    let t = unsafe { GetLocalTime() };
    let stem = format!(
        "sonar-rs {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    let mut path = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).mp4"));
        n += 1;
    }
    path
}
