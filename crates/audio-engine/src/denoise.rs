//! Microphone noise removal: RNNoise for the lightest CPU cost, or
//! DeepFilterNet 3 for much cleaner speech (keyboard clatter, fans and
//! background chatter), plus an optional gate for voices well below yours
//! and a record-then-play test so you can hear the difference without
//! hearing yourself live.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::NoiseSuppression;
use df::tract::{DfParams, DfTract, RuntimeParams};
use nnnoiseless::DenoiseState;

use crate::atomic_f32::AtomicF32;
use crate::error::Result;
use crate::mixer::{SourceInner, MAX_BUFFERED_SAMPLES};
use crate::platform::DeviceOutput;

/// Samples per frame (10 ms at 48 kHz), the native frame of both models.
pub const FRAME: usize = 480;
const SAMPLE_RATE: usize = 48_000;
/// How much Standard may take off: enough to bury keyboards and fans, while
/// leaving a trace of the room so speech doesn't sound cut out.
const STANDARD_ATTENUATION_DB: f32 = 24.0;
/// DeepFilterNet's own value for "no limit".
const UNLIMITED_ATTENUATION_DB: f32 = 100.0;
/// How long the test recording is.
const TEST_TAKE_SECONDS: usize = 6;

fn attenuation_db(mode: NoiseSuppression) -> f32 {
    match mode {
        NoiseSuppression::Strong => UNLIMITED_ATTENUATION_DB,
        _ => STANDARD_ATTENUATION_DB,
    }
}

fn uses_deep_filter(mode: NoiseSuppression) -> bool {
    matches!(mode, NoiseSuppression::Standard | NoiseSuppression::Strong)
}

/// One mic's noise removal, fed 10 ms mono frames in `[-1, 1]`.
pub(crate) enum Denoiser {
    Off,
    Rnnoise {
        state: Box<DenoiseState<'static>>,
        scaled: Vec<f32>,
    },
    DeepFilter(Box<DfTract>),
}

impl Denoiser {
    /// Loading DeepFilterNet takes a moment (it unpacks and optimizes the
    /// model), so call this off the capture thread.
    pub(crate) fn new(mode: NoiseSuppression) -> std::result::Result<Self, String> {
        Ok(match mode {
            NoiseSuppression::Off => Self::Off,
            NoiseSuppression::Light => Self::Rnnoise { state: DenoiseState::new(), scaled: vec![0.0; FRAME] },
            NoiseSuppression::Standard | NoiseSuppression::Strong => {
                // The thresholds upstream's own tools use; the library's
                // defaults skip quiet speech.
                let params = RuntimeParams::default_with_ch(1)
                    .with_atten_lim(attenuation_db(mode))
                    .with_thresholds(-15.0, 35.0, 35.0);
                // `new` also initializes it; initializing twice misaligns
                // its frame history.
                let model = DfTract::new(DfParams::default(), &params).map_err(|e| e.to_string())?;
                Self::DeepFilter(Box::new(model))
            }
        })
    }

    /// Whether this denoiser can do `mode` (Standard and Strong only differ
    /// in a setting); see [`Self::set_mode`].
    fn serves(&self, mode: NoiseSuppression) -> bool {
        match self {
            Self::Off => mode == NoiseSuppression::Off,
            Self::Rnnoise { .. } => mode == NoiseSuppression::Light,
            Self::DeepFilter(_) => uses_deep_filter(mode),
        }
    }

    fn set_mode(&mut self, mode: NoiseSuppression) {
        if let Self::DeepFilter(model) = self {
            model.set_atten_lim(attenuation_db(mode));
        }
    }

    pub(crate) fn process(&mut self, input: &[f32], output: &mut [f32]) {
        match self {
            Self::Off => output.copy_from_slice(input),
            // RNNoise works in 16-bit PCM range.
            Self::Rnnoise { state, scaled } => {
                for (dst, src) in scaled.iter_mut().zip(input) {
                    *dst = src * 32768.0;
                }
                state.process_frame(output, scaled);
                for sample in output.iter_mut() {
                    *sample /= 32768.0;
                }
            }
            Self::DeepFilter(model) => {
                let noisy = ndarray::ArrayView2::from_shape((1, FRAME), input).expect("one frame");
                let enhanced = ndarray::ArrayViewMut2::from_shape((1, FRAME), &mut *output).expect("one frame");
                if model.process(noisy, enhanced).is_err() {
                    output.copy_from_slice(input);
                }
            }
        }
    }
}

/// Turns down speech well below your own level: voices across the room
/// reach the microphone much quieter than yours. It can't tell who is
/// speaking, so someone right next to you, or you whispering, gets through
/// or gets cut the same way.
pub(crate) struct FarVoiceGate {
    /// Your speaking level (dBFS), following loud frames up quickly and
    /// sinking slowly through pauses.
    voice_db: f32,
    gain: f32,
    hold_frames: u32,
}

/// Frames this far below your voice are turned down.
const GATE_BELOW_VOICE_DB: f32 = 15.0;
/// Turned down to this (-20 dB), not silenced, so it never sounds cut out.
const GATE_FLOOR: f32 = 0.1;
/// Your level can't sink below this, so silence doesn't open the gate for
/// anything quiet.
const GATE_VOICE_FLOOR_DB: f32 = -45.0;
/// How fast your remembered level sinks: 1 dB per second.
const GATE_VOICE_DECAY_DB_PER_FRAME: f32 = 0.01;
/// Stays open this long (200 ms) after you, so word endings aren't cut.
const GATE_HOLD_FRAMES: u32 = 20;
/// Closing takes about 50 ms; opening one frame.
const GATE_RELEASE_PER_SAMPLE: f32 = 1.0 / (0.05 * SAMPLE_RATE as f32);

impl FarVoiceGate {
    pub(crate) fn new() -> Self {
        Self { voice_db: -30.0, gain: 1.0, hold_frames: 0 }
    }

    pub(crate) fn process(&mut self, frame: &mut [f32]) {
        let energy = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        let db = 10.0 * (energy + 1e-12).log10();
        if db > self.voice_db {
            self.voice_db += 0.3 * (db - self.voice_db);
        } else {
            self.voice_db = (self.voice_db - GATE_VOICE_DECAY_DB_PER_FRAME).max(GATE_VOICE_FLOOR_DB);
        }
        if db > self.voice_db - GATE_BELOW_VOICE_DB {
            self.hold_frames = GATE_HOLD_FRAMES;
        } else {
            self.hold_frames = self.hold_frames.saturating_sub(1);
        }
        let target = if self.hold_frames > 0 { 1.0 } else { GATE_FLOOR };
        let attack = (1.0 - GATE_FLOOR) / frame.len() as f32;
        for sample in frame.iter_mut() {
            self.gain = if self.gain < target {
                (self.gain + attack).min(target)
            } else {
                (self.gain - GATE_RELEASE_PER_SAMPLE).max(target)
            };
            *sample *= self.gain;
        }
    }
}

/// The microphone's noise removal settings, shared by the UI and the mic's
/// capture thread, and the before/after test.
pub struct MicCleanup {
    mode: AtomicU8,
    quiet_far_voices: AtomicBool,
    test: Mutex<TestTake>,
    playback: Mutex<Option<Arc<Playback>>>,
}

#[derive(Default)]
struct TestTake {
    raw: Vec<f32>,
    recording: bool,
}

struct Playback {
    cleaned: bool,
    stop: AtomicBool,
    progress: AtomicF32,
    done: AtomicBool,
}

/// Where the before/after test is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MicTest {
    /// Nothing recorded yet.
    Empty,
    /// Recording; how far along (0 to 1).
    Recording(f32),
    /// A take is ready to play.
    Ready,
    /// Playing the take as recorded (`cleaned: false`) or cleaned with the
    /// current settings; how far along (0 to 1).
    Playing { cleaned: bool, progress: f32 },
}

fn mode_from_u8(value: u8) -> NoiseSuppression {
    match value {
        0 => NoiseSuppression::Off,
        1 => NoiseSuppression::Light,
        2 => NoiseSuppression::Standard,
        _ => NoiseSuppression::Strong,
    }
}

fn mode_to_u8(mode: NoiseSuppression) -> u8 {
    match mode {
        NoiseSuppression::Off => 0,
        NoiseSuppression::Light => 1,
        NoiseSuppression::Standard => 2,
        NoiseSuppression::Strong => 3,
    }
}

impl Default for MicCleanup {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(mode_to_u8(NoiseSuppression::default())),
            quiet_far_voices: AtomicBool::new(false),
            test: Mutex::default(),
            playback: Mutex::new(None),
        }
    }
}

impl MicCleanup {
    pub fn mode(&self) -> NoiseSuppression {
        mode_from_u8(self.mode.load(Ordering::Relaxed))
    }

    /// Takes effect on the microphone's next frame.
    pub fn set_mode(&self, mode: NoiseSuppression) {
        self.mode.store(mode_to_u8(mode), Ordering::Relaxed);
    }

    pub fn quiet_far_voices(&self) -> bool {
        self.quiet_far_voices.load(Ordering::Relaxed)
    }

    pub fn set_quiet_far_voices(&self, enabled: bool) {
        self.quiet_far_voices.store(enabled, Ordering::Relaxed);
    }

    /// Starts recording a few seconds of the microphone as it comes in,
    /// replacing any earlier take. Nothing is played while recording.
    pub fn record_test(&self) {
        self.stop_test_playback();
        let mut test = self.test.lock().unwrap();
        test.raw = Vec::with_capacity(TEST_TAKE_SECONDS * SAMPLE_RATE);
        test.recording = true;
    }

    pub fn test(&self) -> MicTest {
        if let Some(playback) = self.playback.lock().unwrap().as_ref() {
            if !playback.done.load(Ordering::Relaxed) {
                return MicTest::Playing { cleaned: playback.cleaned, progress: playback.progress.load() };
            }
        }
        let test = self.test.lock().unwrap();
        if test.recording {
            MicTest::Recording(test.raw.len() as f32 / (TEST_TAKE_SECONDS * SAMPLE_RATE) as f32)
        } else if test.raw.is_empty() {
            MicTest::Empty
        } else {
            MicTest::Ready
        }
    }

    /// Plays the test take on the default output device, as recorded or
    /// cleaned with the current settings, once.
    pub fn play_test(&self, cleaned: bool) {
        self.stop_test_playback();
        let raw = {
            let test = self.test.lock().unwrap();
            if test.recording || test.raw.is_empty() {
                return;
            }
            test.raw.clone()
        };
        let playback = Arc::new(Playback {
            cleaned,
            stop: AtomicBool::new(false),
            progress: AtomicF32::new(0.0),
            done: AtomicBool::new(false),
        });
        *self.playback.lock().unwrap() = Some(playback.clone());
        let (mode, gate) = (self.mode(), self.quiet_far_voices());
        std::thread::Builder::new()
            .name("mic-test-playback".into())
            .spawn(move || {
                let audio = if cleaned { clean_take(&raw, mode, gate) } else { raw };
                let _ = play_mono(&audio, &playback);
                playback.done.store(true, Ordering::Relaxed);
            })
            .expect("spawn mic test playback thread");
    }

    pub fn stop_test_playback(&self) {
        if let Some(playback) = self.playback.lock().unwrap().take() {
            playback.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Called by the capture thread with each raw frame.
    pub(crate) fn feed_test(&self, frame: &[f32]) {
        let Ok(mut test) = self.test.try_lock() else { return };
        if !test.recording {
            return;
        }
        let room = TEST_TAKE_SECONDS * SAMPLE_RATE - test.raw.len();
        let take = frame.len().min(room);
        test.raw.extend_from_slice(&frame[..take]);
        if take < frame.len() || test.raw.len() == TEST_TAKE_SECONDS * SAMPLE_RATE {
            test.recording = false;
        }
    }
}

/// Runs `raw` (mono, 48 kHz) through `mode` and, if `gate`, the far-voice
/// gate, exactly as the live microphone would be.
pub fn clean_take(raw: &[f32], mode: NoiseSuppression, gate: bool) -> Vec<f32> {
    let mut denoiser = Denoiser::new(mode).unwrap_or(Denoiser::Off);
    let mut far_voices = gate.then(FarVoiceGate::new);
    let mut out = vec![0.0; raw.len() / FRAME * FRAME];
    for (input, output) in raw.chunks_exact(FRAME).zip(out.chunks_exact_mut(FRAME)) {
        denoiser.process(input, output);
        if let Some(gate) = far_voices.as_mut() {
            gate.process(output);
        }
    }
    out
}

/// The live microphone's processing, switching denoisers when the setting
/// changes. Loading DeepFilterNet happens right on the capture thread (its
/// model can't move between threads); see [`Self::apply_setting`].
pub(crate) struct LiveDenoiser {
    cleanup: Arc<MicCleanup>,
    current: Denoiser,
    /// The mode `current` is set up for.
    mode: NoiseSuppression,
    gate: FarVoiceGate,
}

impl LiveDenoiser {
    pub(crate) fn new(cleanup: Arc<MicCleanup>) -> Self {
        Self { cleanup, current: Denoiser::Off, mode: NoiseSuppression::Off, gate: FarVoiceGate::new() }
    }

    /// Applies a changed setting; call before each batch of frames.
    /// Returns true if it loaded a model, which takes long enough that the
    /// audio captured meanwhile should be dropped rather than played late.
    pub(crate) fn apply_setting(&mut self) -> bool {
        let mode = self.cleanup.mode();
        if mode == self.mode {
            return false;
        }
        let load = !self.current.serves(mode);
        if load {
            // Free the old model before loading the next.
            self.current = Denoiser::Off;
            self.current = Denoiser::new(mode).unwrap_or_else(|_| {
                // Fall back to RNNoise, which has no model to load, rather
                // than retry every frame.
                self.cleanup.set_mode(NoiseSuppression::Light);
                Denoiser::Rnnoise { state: DenoiseState::new(), scaled: vec![0.0; FRAME] }
            });
        }
        self.mode = self.cleanup.mode();
        self.current.set_mode(self.mode);
        load
    }

    pub(crate) fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.cleanup.feed_test(input);
        self.current.process(input, output);
        if self.cleanup.quiet_far_voices() {
            self.gate.process(output);
        }
    }
}

/// A microphone's audio on its way into the mix: cut into 10 ms frames,
/// cleaned up, and pushed (duplicated to stereo) into its source's queue,
/// exactly like an app's audio. Each platform's capture feeds it.
pub(crate) struct MicPipeline {
    denoiser: LiveDenoiser,
    staging: Vec<f32>,
    out_frame: Vec<f32>,
}

impl MicPipeline {
    pub(crate) fn new(cleanup: Arc<MicCleanup>) -> Self {
        Self { denoiser: LiveDenoiser::new(cleanup), staging: Vec::with_capacity(FRAME * 4), out_frame: vec![0.0; FRAME] }
    }

    /// Applies a changed setting; call before each batch of captured audio.
    /// Returns true if the batch should be dropped: loading a model takes a
    /// moment, during which audio piles up, and playing it would leave the
    /// mic late from then on.
    pub(crate) fn begin_batch(&mut self) -> bool {
        let discard = self.denoiser.apply_setting();
        if discard {
            self.staging.clear();
        }
        discard
    }

    /// Takes mono 48 kHz samples as captured.
    pub(crate) fn push(&mut self, mono: &[f32], source: &SourceInner) {
        self.staging.extend_from_slice(mono);
        while self.staging.len() >= FRAME {
            self.denoiser.process(&self.staging[..FRAME], &mut self.out_frame);
            self.staging.drain(..FRAME);
            let mut buffer = source.samples.lock().unwrap();
            for &cleaned in &self.out_frame {
                let sample = cleaned.clamp(-1.0, 1.0);
                buffer.push_back(sample);
                buffer.push_back(sample);
            }
            while buffer.len() > MAX_BUFFERED_SAMPLES {
                buffer.pop_front();
            }
        }
    }
}

/// Plays mono 48 kHz `audio` on the default output device.
fn play_mono(audio: &[f32], playback: &Playback) -> Result<()> {
    let _thread = crate::platform::audio_thread()?;
    let mut output = DeviceOutput::open(None)?;
    // A quarter second queued at a time: plenty, and stopping is quick.
    let chunk = (SAMPLE_RATE / 4) as u32;
    let mut stereo = Vec::new();
    let mut position = 0;
    while position < audio.len() && !playback.stop.load(Ordering::Relaxed) {
        let room = output.capacity_frames().min(chunk).saturating_sub(output.queued_frames()?);
        let frames = (room as usize).min(audio.len() - position);
        if frames > 0 {
            stereo.clear();
            stereo.extend(audio[position..position + frames].iter().flat_map(|&s| [s, s]));
            output.write(&stereo)?;
            position += frames;
            playback.progress.store(position as f32 / audio.len() as f32);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Let what's queued finish playing.
    while !playback.stop.load(Ordering::Relaxed) && output.queued_frames()? > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn off_leaves_the_microphone_untouched() {
        let raw = tone(0.3, 0.5);
        assert_eq!(clean_take(&raw, NoiseSuppression::Off, false), raw);
    }

    #[test]
    fn far_voice_gate_turns_down_what_is_much_quieter_than_you() {
        let mut take = tone(0.3, 1.0); // you, close to the mic
        take.extend(tone(0.01, 1.0)); // someone across the room, -30 dB
        let out = clean_take(&take, NoiseSuppression::Off, true);
        let you = rms(&out[24_000..48_000]) / rms(&take[24_000..48_000]);
        let them = rms(&out[72_000..96_000]) / rms(&take[72_000..96_000]);
        assert!(you > 0.99, "your voice kept: {you}");
        assert!(them < GATE_FLOOR * 1.01, "far voice turned down: {them}");
    }

    #[test]
    fn far_voice_gate_keeps_voices_near_yours() {
        let mut take = tone(0.3, 1.0);
        take.extend(tone(0.1, 1.0)); // -10 dB: still you, or someone close
        let out = clean_take(&take, NoiseSuppression::Off, true);
        assert!(rms(&out[72_000..96_000]) / rms(&take[72_000..96_000]) > 0.99);
    }

    #[test]
    fn deep_filter_loads_and_keeps_frame_length() {
        let raw = tone(0.1, 0.25);
        let out = clean_take(&raw, NoiseSuppression::Standard, false);
        assert_eq!(out.len(), raw.len() / FRAME * FRAME);
    }
}
