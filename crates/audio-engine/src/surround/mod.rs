//! Virtual surround for headphones: a channel played as if from speakers in
//! a room around you, rather than inside your head.
//!
//! Each speaker, and each wall's reflection of it, reaches your ears the way
//! sound from its direction reaches a dummy head's (the measured responses in
//! `hrir.rs`), and a short reverb fills in the rest of the room. Apps send
//! stereo, so there are two speakers, 30° to either side in front of you.
//! Farther speakers stand in a bigger room: their reflections arrive later
//! but louder next to the direct sound, and there's more reverb. The overall
//! level stays about the same, so only the sense of distance changes.

mod hrir;

use std::f32::consts::FRAC_PI_2;

use common::Surround;

use crate::mixer::SAMPLE_RATE;
use hrir::{Hrir, HRIRS, TAPS};

/// Directions sound can come from: those measured in [`HRIRS`].
const DIRECTIONS: usize = HRIRS.len();
/// Where the speakers are, in degrees anticlockwise from straight ahead (left,
/// then right).
const SPEAKER_AZIMUTHS: [f32; 2] = [30.0, 330.0];
const SPEED_OF_SOUND: f32 = 343.0;
/// How far the walls are behind the speakers.
const WALL_GAP_M: f32 = 1.0;
/// How much of the sound reaching a wall comes back (half its energy).
const WALL_REFLECTION: f32 = 0.7;
/// The four walls around you: floor and ceiling reflections would come from
/// above and below, which the responses don't cover.
const WALLS: usize = 4;
const REFLECTIONS: usize = SPEAKER_AZIMUTHS.len() * WALLS;
/// Walls soak up highs: each reflection goes through a one-pole low-pass
/// at about 7 kHz.
const WALL_DAMPING: f32 = 0.6;
/// The reverb's delay lines, in frames (12 to 30 ms): mutually prime, so
/// their echoes don't line up into a ringing pitch.
const REVERB_LINES: [usize; DIRECTIONS] = [601, 701, 829, 953, 1069, 1201, 1327, 1453];
/// How long the room rings (to a thousandth), and how loud the reverb is
/// next to the direct sound, from the closest speakers to the farthest.
const RING_SECONDS: (f32, f32) = (0.25, 0.55);
const REVERB_DB: (f32, f32) = (-22.0, -6.0);
/// Recent input kept for the reflections and the reverb to read back; a
/// power of two, comfortably over the latest reflection at the largest
/// distance (about 1,350 frames).
const INPUT_FRAMES: usize = 4096;
/// How long turning surround on or off takes, crossfading so it doesn't
/// click.
const FADE_SECONDS: f32 = 0.03;
/// How quickly a new distance is glided to, so moving the slider doesn't
/// click (slower bends the reflections' pitch less on the way), and how long
/// until it's reached.
const GLIDE_SECONDS: f32 = 0.05;
const SETTLE_SECONDS: f32 = 0.4;
/// Below this (about -180 dB) the input counts as silence; after this long
/// of it the room has died away, and surround stops working until sound
/// comes back.
const SILENCE: f32 = 1e-9;
const IDLE_AFTER_SECONDS: f32 = 2.0;

/// One channel's virtual surround: on or off, and how far away the speakers
/// are ([`Surround`]). Off, it leaves the sound untouched and costs nothing.
pub struct VirtualSurround {
    settings: Surround,
    /// How much of the output is surround: glides to 1 when on, 0 when off.
    mix: f32,
    /// Made the first time surround is turned on, so channels that never use
    /// it don't carry its buffers.
    state: Option<Box<State>>,
}

impl VirtualSurround {
    /// Starts as `settings` say, already fully on if they're on.
    pub fn new(settings: Surround) -> Self {
        let mut surround = Self { settings: Surround::default(), mix: 0.0, state: None };
        surround.set(settings);
        if surround.settings.enabled {
            surround.mix = 1.0;
        }
        surround
    }

    pub fn settings(&self) -> Surround {
        self.settings
    }

    /// Turns surround on or off (crossfading) and sets the speakers' distance
    /// (gliding to it). A distance outside the range is brought into it.
    pub fn set(&mut self, settings: Surround) {
        let settings = Surround { enabled: settings.enabled, distance_m: clamp_distance(settings.distance_m) };
        if settings.enabled && self.state.is_none() {
            self.state = Some(Box::new(State::new(settings.distance_m)));
        }
        if let Some(state) = &mut self.state {
            if settings.distance_m != self.settings.distance_m {
                state.target = Layout::for_distance(settings.distance_m);
                if self.mix > 0.0 {
                    state.settling = (SETTLE_SECONDS * SAMPLE_RATE) as u32;
                } else {
                    state.layout = state.target;
                }
            }
        }
        self.settings = settings;
    }

    /// Call instead of [`Self::process`] for frames nobody hears (a muted
    /// channel): the next frame heard starts from a quiet room, rather than
    /// echoing what played before.
    pub fn skip(&mut self) {
        if let Some(state) = &mut self.state {
            state.stale = true;
        }
    }

    /// Processes one interleaved stereo frame in place.
    pub fn process(&mut self, frame: &mut [f32; 2]) {
        let on = self.settings.enabled;
        if !on && self.mix == 0.0 {
            return;
        }
        let Some(state) = &mut self.state else { return };
        if state.stale {
            state.clear();
        }
        let fading = self.mix != if on { 1.0 } else { 0.0 };
        if frame.iter().all(|s| s.abs() < SILENCE) && !fading {
            // Once the room has died away there's nothing to add to silence.
            let idle_after = (IDLE_AFTER_SECONDS * SAMPLE_RATE) as u32;
            if state.quiet_frames > idle_after {
                return;
            }
            state.quiet_frames += 1;
            if state.quiet_frames > idle_after {
                state.clear();
                return;
            }
        } else {
            state.quiet_frames = 0;
        }

        let step = 1.0 / (FADE_SECONDS * SAMPLE_RATE);
        self.mix = if on { (self.mix + step).min(1.0) } else { (self.mix - step).max(0.0) };
        let wet = state.render(*frame);
        for (out, wet) in frame.iter_mut().zip(wet) {
            *out += (wet - *out) * self.mix;
        }
        if self.mix == 0.0 {
            // Off: the next time it's turned on starts from a quiet room.
            state.clear();
        }
    }
}

fn clamp_distance(distance_m: f32) -> f32 {
    if distance_m.is_finite() {
        distance_m.clamp(Surround::MIN_DISTANCE_M, Surround::MAX_DISTANCE_M)
    } else {
        Surround::DEFAULT_DISTANCE_M
    }
}

/// Everything that depends on the distance. Glided toward when it changes,
/// so the reflections slide to their new places instead of jumping (which
/// would click).
#[derive(Clone, Copy)]
struct Layout {
    /// The direct sound's gain, which also sets the overall level.
    direct: f32,
    /// Each reflection (each speaker's, wall by wall): how long after the
    /// direct sound it arrives, in frames, how loud it is, and how much of it
    /// comes from each direction.
    delay: [f32; REFLECTIONS],
    gain: [f32; REFLECTIONS],
    pan: [[f32; DIRECTIONS]; REFLECTIONS],
    /// How much goes into the reverb, and how long after the direct sound.
    reverb_send: f32,
    reverb_delay: f32,
    /// Each reverb line's feedback, which sets how long the room rings,
    /// and its low-pass, which makes highs die away twice as fast, like in a
    /// furnished room.
    feedback: [f32; DIRECTIONS],
    damping: [f32; DIRECTIONS],
}

impl Layout {
    /// The speakers `distance_m` away, in a square room with you in the middle
    /// and the walls [`WALL_GAP_M`] behind them. Each wall's reflection comes
    /// from the speaker's mirror image in it.
    fn for_distance(distance_m: f32) -> Self {
        let half_room = distance_m + WALL_GAP_M;
        let mut delay = [0.0; REFLECTIONS];
        let mut gain = [0.0; REFLECTIONS];
        let mut pan = [[0.0; DIRECTIONS]; REFLECTIONS];
        for (speaker, azimuth) in SPEAKER_AZIMUTHS.iter().enumerate() {
            // x ahead of you, y to your left.
            let (y, x) = azimuth.to_radians().sin_cos();
            let (x, y) = (x * distance_m, y * distance_m);
            let images = [(2.0 * half_room - x, y), (-2.0 * half_room - x, y), (x, 2.0 * half_room - y), (x, -2.0 * half_room - y)];
            for (wall, (image_x, image_y)) in images.into_iter().enumerate() {
                let reflection = speaker * WALLS + wall;
                let path = image_x.hypot(image_y);
                delay[reflection] = (path - distance_m) / SPEED_OF_SOUND * SAMPLE_RATE;
                gain[reflection] = WALL_REFLECTION * distance_m / path;
                pan[reflection] = pan_weights(image_y.atan2(image_x).to_degrees());
            }
        }

        let closeness = (distance_m - Surround::MIN_DISTANCE_M) / (Surround::MAX_DISTANCE_M - Surround::MIN_DISTANCE_M);
        let ring_seconds = lerp(RING_SECONDS, closeness);
        let reverb_energy = 10f32.powf(lerp(REVERB_DB, closeness) / 10.0);
        let feedback = REVERB_LINES.map(|frames| 10f32.powf(-3.0 * frames as f32 / (ring_seconds * SAMPLE_RATE)));
        // A one-pole low-pass passing all of the lows and `feedback` of the
        // highest highs, so they lose twice as much each pass.
        let damping = feedback.map(|g| (1.0 - g) / (1.0 + g));
        // Each pass round the reverb keeps `feedback²` of the lows' energy,
        // so they come back about 1 / (1 - feedback²) times over; the highest
        // highs keep `feedback⁴`. Taken as halfway between. The reverb's
        // input is the speakers' average, carrying a quarter of their energy
        // together when they differ.
        let sustain = feedback.iter().map(|g| (1.0 / (1.0 - g * g) + 1.0 / (1.0 - g.powi(4))) / 2.0).sum::<f32>() / DIRECTIONS as f32;
        let reverb_send = (4.0 * reverb_energy / sustain).sqrt();

        // What one speaker's reflections add to it, the same for both, after
        // the walls' low-pass (which keeps WALL_DAMPING² / (1 - (1 -
        // WALL_DAMPING)²) of white noise's energy).
        let wall_kept = WALL_DAMPING * WALL_DAMPING / (1.0 - (1.0 - WALL_DAMPING) * (1.0 - WALL_DAMPING));
        let reflected = wall_kept * gain[..WALLS].iter().map(|g| g * g).sum::<f32>();
        let level = 1.0 / (1.0 + reflected + reverb_energy).sqrt();
        Self {
            direct: level,
            delay,
            gain: gain.map(|g| g * level),
            pan,
            reverb_send: reverb_send * level,
            reverb_delay: delay.iter().copied().fold(0.0, f32::max),
            feedback,
            damping,
        }
    }

    /// Moves every value `amount` (0 to 1) of the way to `target`'s.
    fn glide_toward(&mut self, target: &Self, amount: f32) {
        let glide = |value: &mut f32, target: f32| *value += (target - *value) * amount;
        glide(&mut self.direct, target.direct);
        glide(&mut self.reverb_send, target.reverb_send);
        glide(&mut self.reverb_delay, target.reverb_delay);
        for reflection in 0..REFLECTIONS {
            glide(&mut self.delay[reflection], target.delay[reflection]);
            glide(&mut self.gain[reflection], target.gain[reflection]);
            for direction in 0..DIRECTIONS {
                glide(&mut self.pan[reflection][direction], target.pan[reflection][direction]);
            }
        }
        for line in 0..DIRECTIONS {
            glide(&mut self.feedback[line], target.feedback[line]);
            glide(&mut self.damping[line], target.damping[line]);
        }
    }
}

fn lerp((from, to): (f32, f32), amount: f32) -> f32 {
    from + (to - from) * amount
}

/// How much of a sound from `azimuth` (degrees anticlockwise from straight
/// ahead) each measured direction gets: shared between the two either side
/// of it, keeping its power.
fn pan_weights(azimuth: f32) -> [f32; DIRECTIONS] {
    let mut azimuth = azimuth.rem_euclid(360.0);
    if azimuth >= 360.0 {
        // rem_euclid rounds a hair below zero up to 360.
        azimuth = 0.0;
    }
    let mut weights = [0.0; DIRECTIONS];
    for (index, hrir) in HRIRS.iter().enumerate() {
        let next = (index + 1) % DIRECTIONS;
        let until = if next == 0 { 360.0 } else { HRIRS[next].azimuth };
        if (hrir.azimuth..until).contains(&azimuth) {
            let along = (azimuth - hrir.azimuth) / (until - hrir.azimuth) * FRAC_PI_2;
            weights[index] = along.cos();
            weights[next] = along.sin();
        }
    }
    weights
}

/// Surround's buffers and filters, made when it's first turned on.
struct State {
    layout: Layout,
    target: Layout,
    /// Frames left gliding `layout` to `target`.
    settling: u32,
    /// Each speaker's recent input, for the reflections and the reverb.
    input: [Box<[f32; INPUT_FRAMES]>; 2],
    /// Frames written to `input` (wrapping).
    written: usize,
    /// Each reflection's low-pass (walls soaking up highs).
    walls: [f32; REFLECTIONS],
    reverb: Reverb,
    /// What each direction was fed recently, for convolving with its
    /// responses.
    feeds: [History; DIRECTIONS],
    quiet_frames: u32,
    /// Set when frames were skipped: start again from a quiet room.
    stale: bool,
}

impl State {
    fn new(distance_m: f32) -> Self {
        let layout = Layout::for_distance(distance_m);
        Self {
            layout,
            target: layout,
            settling: 0,
            input: [Box::new([0.0; INPUT_FRAMES]), Box::new([0.0; INPUT_FRAMES])],
            written: 0,
            walls: [0.0; REFLECTIONS],
            reverb: Reverb::new(),
            feeds: std::array::from_fn(|_| History::default()),
            quiet_frames: 0,
            stale: false,
        }
    }

    /// Silences the room (keeping the layout).
    fn clear(&mut self) {
        for input in &mut self.input {
            input.fill(0.0);
        }
        self.walls = [0.0; REFLECTIONS];
        self.reverb.clear();
        for feed in &mut self.feeds {
            *feed = History::default();
        }
        self.layout = self.target;
        self.settling = 0;
        self.stale = false;
    }

    /// One frame of `[left, right]` as heard from the speakers: what reaches
    /// each ear.
    fn render(&mut self, [left, right]: [f32; 2]) -> [f32; 2] {
        if self.settling > 0 {
            self.settling -= 1;
            if self.settling == 0 {
                self.layout = self.target;
            } else {
                let amount = 1.0 - (-1.0 / (GLIDE_SECONDS * SAMPLE_RATE)).exp();
                self.layout.glide_toward(&self.target, amount);
            }
        }
        let at = self.written % INPUT_FRAMES;
        self.input[0][at] = left;
        self.input[1][at] = right;
        self.written = self.written.wrapping_add(1);
        let layout = &self.layout;

        // What arrives from each direction: the speakers themselves...
        let mut feed = [0.0f32; DIRECTIONS];
        feed[SPEAKER_DIRECTIONS[0]] += left * layout.direct;
        feed[SPEAKER_DIRECTIONS[1]] += right * layout.direct;
        // ...each wall's reflection of them...
        for reflection in 0..REFLECTIONS {
            let heard = self.read(reflection / WALLS, layout.delay[reflection]) * layout.gain[reflection];
            let wall = &mut self.walls[reflection];
            *wall += (heard - *wall) * WALL_DAMPING;
            for (feed, pan) in feed.iter_mut().zip(&layout.pan[reflection]) {
                *feed += *wall * pan;
            }
        }
        // ...and the reverb, all around.
        let reverb_in = (self.read(0, layout.reverb_delay) + self.read(1, layout.reverb_delay)) * 0.5 * layout.reverb_send;
        for (feed, tail) in feed.iter_mut().zip(self.reverb.process(reverb_in, &layout.feedback, &layout.damping)) {
            *feed += tail;
        }

        let mut ears = [0.0f32; 2];
        for ((history, hrir), feed) in self.feeds.iter_mut().zip(&HRIRS).zip(feed) {
            let [left, right] = convolve(history.push(feed), hrir);
            ears[0] += left;
            ears[1] += right;
        }
        ears
    }

    /// `channel`'s input `delay` frames before the newest (fractions in
    /// between two frames).
    fn read(&self, channel: usize, delay: f32) -> f32 {
        let whole = delay as usize;
        let newest = self.written.wrapping_sub(1).wrapping_sub(whole);
        let input = &self.input[channel];
        let near = input[newest % INPUT_FRAMES];
        let far = input[newest.wrapping_sub(1) % INPUT_FRAMES];
        near + (far - near) * (delay - whole as f32)
    }
}

/// Which of [`HRIRS`] each speaker is at.
const SPEAKER_DIRECTIONS: [usize; 2] = [direction_at(SPEAKER_AZIMUTHS[0]), direction_at(SPEAKER_AZIMUTHS[1])];

const fn direction_at(azimuth: f32) -> usize {
    let mut index = 0;
    while index < DIRECTIONS {
        if HRIRS[index].azimuth == azimuth {
            return index;
        }
        index += 1;
    }
    panic!("speakers must stand where responses were measured");
}

/// A direction's responses applied to what it was fed: what each ear hears
/// from it now.
fn convolve(history: &[f32; TAPS], hrir: &Hrir) -> [f32; 2] {
    // Eight running sums per ear, so the compiler can use vector
    // instructions (floating-point sums can't be reordered otherwise).
    let mut left = [0.0f32; 8];
    let mut right = [0.0f32; 8];
    let chunks = history.as_chunks::<8>().0.iter().zip(hrir.left.as_chunks::<8>().0).zip(hrir.right.as_chunks::<8>().0);
    for ((fed, to_left), to_right) in chunks {
        for lane in 0..8 {
            left[lane] += fed[lane] * to_left[lane];
            right[lane] += fed[lane] * to_right[lane];
        }
    }
    [left.iter().sum(), right.iter().sum()]
}

/// The last [`TAPS`] samples fed to a direction, newest first. Kept twice
/// over so they're always one unbroken slice.
struct History {
    samples: [f32; 2 * TAPS],
    newest: usize,
}

impl Default for History {
    fn default() -> Self {
        Self { samples: [0.0; 2 * TAPS], newest: 0 }
    }
}

impl History {
    /// Adds `sample` and returns the last [`TAPS`], newest first.
    fn push(&mut self, sample: f32) -> &[f32; TAPS] {
        self.newest = if self.newest == 0 { TAPS - 1 } else { self.newest - 1 };
        self.samples[self.newest] = sample;
        self.samples[self.newest + TAPS] = sample;
        self.samples[self.newest..self.newest + TAPS].try_into().expect("TAPS long")
    }
}

/// A feedback delay network: eight delay lines feeding back into each other
/// through a mixing matrix that keeps their energy, so echoes multiply into
/// a smooth tail. Each line's output comes from its own direction.
struct Reverb {
    lines: [Vec<f32>; DIRECTIONS],
    at: [usize; DIRECTIONS],
    /// Each line's low-pass (highs dying away faster).
    damped: [f32; DIRECTIONS],
}

impl Reverb {
    fn new() -> Self {
        Self { lines: REVERB_LINES.map(|frames| vec![0.0; frames]), at: [0; DIRECTIONS], damped: [0.0; DIRECTIONS] }
    }

    fn clear(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.damped = [0.0; DIRECTIONS];
    }

    /// Feeds in one sample; returns what each direction hears of the tail.
    fn process(&mut self, input: f32, feedback: &[f32; DIRECTIONS], damping: &[f32; DIRECTIONS]) -> [f32; DIRECTIONS] {
        for (((damped, line), &at), damping) in self.damped.iter_mut().zip(&self.lines).zip(&self.at).zip(damping) {
            let delayed = line[at];
            *damped = delayed + (*damped - delayed) * damping;
        }
        let out = self.damped;
        let mixed = hadamard(out);
        // The input reaches every line, at alternating signs so the lines
        // don't start out alike, keeping its energy overall.
        let share = input * (1.0 / DIRECTIONS as f32).sqrt();
        for line in 0..DIRECTIONS {
            let signed = if line % 2 == 0 { share } else { -share };
            self.lines[line][self.at[line]] = signed + mixed[line] * feedback[line];
            self.at[line] = (self.at[line] + 1) % REVERB_LINES[line];
        }
        out
    }
}

/// Mixes eight values with the 8×8 Hadamard matrix, scaled to keep their
/// energy: each output is every input, at a different pattern of signs.
fn hadamard(mut values: [f32; 8]) -> [f32; 8] {
    let mut span = 1;
    while span < 8 {
        for start in (0..8).step_by(2 * span) {
            for i in start..start + span {
                let (a, b) = (values[i], values[i + span]);
                values[i] = a + b;
                values[i + span] = a - b;
            }
        }
        span *= 2;
    }
    values.map(|v| v * (1.0 / 8f32.sqrt()))
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    const SECOND: usize = SAMPLE_RATE as usize;

    fn on(distance_m: f32) -> Surround {
        Surround { enabled: true, distance_m }
    }

    fn run(surround: &mut VirtualSurround, input: impl IntoIterator<Item = [f32; 2]>) -> Vec<[f32; 2]> {
        input
            .into_iter()
            .map(|mut frame| {
                surround.process(&mut frame);
                frame
            })
            .collect()
    }

    /// What `input` alone sounds like through surround from a quiet room.
    fn response(distance_m: f32, input: [f32; 2], frames: usize) -> Vec<[f32; 2]> {
        let mut surround = VirtualSurround::new(on(distance_m));
        run(&mut surround, std::iter::once(input).chain(std::iter::repeat([0.0; 2])).take(frames))
    }

    fn energy(frames: &[[f32; 2]], ear: usize) -> f32 {
        frames.iter().map(|f| f[ear] * f[ear]).sum()
    }

    fn db(ratio: f32) -> f32 {
        10.0 * ratio.log10()
    }

    /// Repeatable white noise, independent in each channel.
    fn noise(frames: usize, amplitude: f32) -> Vec<[f32; 2]> {
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * amplitude
        };
        (0..frames).map(|_| [next(), next()]).collect()
    }

    fn sine(frames: usize) -> impl Iterator<Item = [f32; 2]> {
        (0..frames).map(|n| {
            let s = 0.5 * (TAU * 440.0 * n as f32 / SAMPLE_RATE).sin();
            [s, s]
        })
    }

    fn largest_jump(frames: &[[f32; 2]]) -> f32 {
        frames.windows(2).flat_map(|w| [(w[1][0] - w[0][0]).abs(), (w[1][1] - w[0][1]).abs()]).fold(0.0, f32::max)
    }

    #[test]
    fn off_leaves_the_sound_untouched() {
        let mut surround = VirtualSurround::new(Surround::default());
        let input = noise(SECOND, 0.5);
        assert_eq!(run(&mut surround, input.iter().copied()), input);
        assert!(surround.state.is_none(), "off from the start keeps no buffers");
    }

    #[test]
    fn the_left_speaker_is_louder_and_sooner_in_the_left_ear() {
        let heard = response(Surround::DEFAULT_DISTANCE_M, [1.0, 0.0], 256);
        let difference = db(energy(&heard, 0) / energy(&heard, 1));
        assert!(difference > 6.0, "left ear only {difference:.1} dB louder");
        let arrival = |ear: usize| {
            let peak = heard.iter().map(|f| f[ear].abs()).fold(0.0, f32::max);
            heard.iter().position(|f| f[ear].abs() > 0.3 * peak).unwrap()
        };
        // About 0.2 ms: sound going round your head to the far ear.
        assert!(arrival(1) > arrival(0) + 4, "left at {}, right at {}", arrival(0), arrival(1));
    }

    #[test]
    fn the_right_speaker_mirrors_the_left() {
        let heard = response(Surround::DEFAULT_DISTANCE_M, [0.0, 1.0], 256);
        assert!(db(energy(&heard, 1) / energy(&heard, 0)) > 6.0);
    }

    #[test]
    fn sound_in_both_speakers_is_centered() {
        let heard = response(Surround::DEFAULT_DISTANCE_M, [1.0, 1.0], SECOND);
        let balance = db(energy(&heard, 0) / energy(&heard, 1));
        assert!(balance.abs() < 1.5, "{balance:.1} dB off center");
    }

    #[test]
    fn farther_speakers_bring_more_of_the_room() {
        // The direct sound is over within 3 ms (HRIRs are 2.7 ms long); the
        // room is everything after.
        let direct = 144;
        let room_vs_direct = |distance_m: f32| {
            let heard = response(distance_m, [1.0, 0.0], SECOND);
            db((energy(&heard[direct..], 0) + energy(&heard[direct..], 1)) / (energy(&heard[..direct], 0) + energy(&heard[..direct], 1)))
        };
        let close = room_vs_direct(Surround::MIN_DISTANCE_M);
        let middle = room_vs_direct(Surround::DEFAULT_DISTANCE_M);
        let far = room_vs_direct(Surround::MAX_DISTANCE_M);
        assert!(close + 3.0 < middle && middle + 3.0 < far, "room at close {close:.1} dB, middle {middle:.1}, far {far:.1}");
    }

    #[test]
    fn farther_reflections_arrive_later() {
        let first_reflection = |distance_m: f32| Layout::for_distance(distance_m).delay.iter().copied().fold(f32::MAX, f32::min);
        assert!(first_reflection(Surround::MAX_DISTANCE_M) > first_reflection(Surround::MIN_DISTANCE_M));
    }

    #[test]
    fn loudness_stays_about_the_same() {
        let input = noise(3 * SECOND, 0.25);
        let rms = |frames: &[[f32; 2]]| (frames.iter().map(|f| f[0] * f[0] + f[1] * f[1]).sum::<f32>() / frames.len() as f32).sqrt();
        for distance_m in [Surround::MIN_DISTANCE_M, Surround::DEFAULT_DISTANCE_M, Surround::MAX_DISTANCE_M] {
            let mut surround = VirtualSurround::new(on(distance_m));
            let heard = run(&mut surround, input.iter().copied());
            // After the room has filled up.
            let change = 20.0 * (rms(&heard[SECOND..]) / rms(&input[SECOND..])).log10();
            assert!(change.abs() < 2.0, "{change:.1} dB louder at {distance_m} m");
        }
    }

    /// The most a steady sine moves from one frame to the next, through
    /// surround set to `settings`.
    fn steady_jump(settings: Surround) -> f32 {
        let mut surround = VirtualSurround::new(settings);
        largest_jump(&run(&mut surround, sine(SECOND))[SECOND / 2..])
    }

    #[test]
    fn turning_it_on_and_off_fades_without_clicks() {
        let normal_jump = steady_jump(Surround::default()).max(steady_jump(on(Surround::DEFAULT_DISTANCE_M)));
        let mut surround = VirtualSurround::new(Surround::default());
        let mut frames = sine(4 * SECOND);
        run(&mut surround, frames.by_ref().take(SECOND));
        surround.set(on(Surround::DEFAULT_DISTANCE_M));
        let turning_on = run(&mut surround, frames.by_ref().take(SECOND));
        surround.set(Surround { enabled: false, ..surround.settings() });
        let turning_off = run(&mut surround, frames.by_ref().take(SECOND));
        for (what, heard) in [("on", &turning_on), ("off", &turning_off)] {
            let jump = largest_jump(heard);
            assert!(jump < 2.0 * normal_jump, "turning {what} jumped {jump}, a sine moves {normal_jump}");
        }
        // Fully off again: untouched.
        let after = frames.next().unwrap();
        let mut frame = after;
        surround.process(&mut frame);
        assert_eq!(frame, after);
    }

    #[test]
    fn moving_the_distance_glides_without_clicks() {
        let normal_jump = steady_jump(on(Surround::MIN_DISTANCE_M)).max(steady_jump(on(Surround::MAX_DISTANCE_M)));
        let mut surround = VirtualSurround::new(on(Surround::MIN_DISTANCE_M));
        let mut frames = sine(3 * SECOND);
        run(&mut surround, frames.by_ref().take(SECOND));
        surround.set(on(Surround::MAX_DISTANCE_M));
        let moving = run(&mut surround, frames.by_ref().take(SECOND));
        let jump = largest_jump(&moving);
        assert!(jump < 2.0 * normal_jump, "moving jumped {jump}, a sine moves {normal_jump}");
    }

    #[test]
    fn loud_noise_stays_finite_and_in_range() {
        let mut surround = VirtualSurround::new(on(Surround::MAX_DISTANCE_M));
        let heard = run(&mut surround, noise(10 * SECOND, 1.0));
        assert!(heard.iter().flatten().all(|s| s.is_finite() && s.abs() < 4.0));
    }

    #[test]
    fn silence_stops_the_work_once_the_room_dies_away() {
        let mut surround = VirtualSurround::new(on(Surround::MAX_DISTANCE_M));
        run(&mut surround, noise(SECOND, 0.5));
        let tail = run(&mut surround, std::iter::repeat([0.0; 2]).take(3 * SECOND));
        assert!(tail[..SECOND / 10].iter().flatten().any(|s| s.abs() > 1e-4), "the room rings on after the sound");
        assert!(tail[5 * SECOND / 2..].iter().flatten().all(|&s| s == 0.0), "then it's silent");
        assert!(surround.state.as_ref().unwrap().quiet_frames as usize >= 2 * SECOND);
    }

    #[test]
    fn skipped_frames_dont_echo_later() {
        let mut surround = VirtualSurround::new(on(Surround::MAX_DISTANCE_M));
        run(&mut surround, noise(SECOND, 0.5));
        surround.skip();
        let after = run(&mut surround, std::iter::repeat([0.0; 2]).take(SECOND / 10));
        assert!(after.iter().flatten().all(|&s| s == 0.0));
    }

    #[test]
    fn distances_outside_the_range_are_brought_in() {
        let mut surround = VirtualSurround::new(on(100.0));
        assert_eq!(surround.settings().distance_m, Surround::MAX_DISTANCE_M);
        surround.set(on(f32::NAN));
        assert_eq!(surround.settings().distance_m, Surround::DEFAULT_DISTANCE_M);
        surround.set(on(0.0));
        assert_eq!(surround.settings().distance_m, Surround::MIN_DISTANCE_M);
    }

    #[test]
    fn reflections_fit_in_the_input_kept() {
        let layout = Layout::for_distance(Surround::MAX_DISTANCE_M);
        let latest = layout.delay.iter().copied().fold(layout.reverb_delay, f32::max);
        assert!(latest < (INPUT_FRAMES - 2) as f32, "{latest} frames");
    }

    #[test]
    fn panning_keeps_power_and_lands_on_measured_directions() {
        assert_eq!(HRIRS[0].azimuth, 0.0, "panning wraps from the last direction to 0°");
        assert!(HRIRS.windows(2).all(|w| w[0].azimuth < w[1].azimuth));
        for azimuth in [-0.0001, 0.0, 15.0, 30.0, 61.0, 179.9, 345.0, 359.99, 720.0] {
            let weights = pan_weights(azimuth);
            let power: f32 = weights.iter().map(|w| w * w).sum();
            assert!((power - 1.0).abs() < 1e-5, "{azimuth}°: power {power}");
        }
        assert_eq!(pan_weights(30.0)[SPEAKER_DIRECTIONS[0]], 1.0);
    }

    #[test]
    fn hadamard_keeps_energy() {
        let values = [0.3, -1.0, 0.5, 0.25, 0.0, 2.0, -0.7, 0.1];
        let energy = |v: &[f32; 8]| v.iter().map(|x| x * x).sum::<f32>();
        assert!((energy(&hadamard(values)) - energy(&values)).abs() < 1e-5);
    }

    /// How much of one CPU core a channel with surround on takes:
    /// `cargo test --release -p audio-engine --lib surround::tests::cpu_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cpu_cost() {
        let mut surround = VirtualSurround::new(on(Surround::DEFAULT_DISTANCE_M));
        let input = noise(10 * SECOND, 0.5);
        let started = std::time::Instant::now();
        let heard = run(&mut surround, input.iter().copied());
        let elapsed = started.elapsed();
        std::hint::black_box(heard);
        println!("10 s of audio in {elapsed:?}: {:.2}% of one core", elapsed.as_secs_f64() / 10.0 * 100.0);
    }
}
