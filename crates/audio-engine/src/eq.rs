//! A channel's equalizer: ten bands, each a peak, a shelf or a cut placed
//! anywhere from 20 Hz to 20 kHz, run in series. Each band is an RBJ
//! cookbook biquad; one that doesn't change the sound is skipped.

use std::f32::consts::PI;

pub use common::{EqBand, EqKind, DEFAULT_EQ_BANDS, EQ_BANDS};

pub const EQ_GAIN_RANGE_DB: f32 = 12.0;
/// Where a band can be, in Hz.
pub const EQ_FREQ_RANGE: (f32, f32) = (20.0, 20_000.0);
/// How wide (low Q) or narrow (high Q) a band can be.
pub const EQ_Q_RANGE: (f32, f32) = (0.3, 10.0);

/// A band kept within the ranges it can take.
pub fn clamped(band: EqBand) -> EqBand {
    EqBand {
        freq: band.freq.clamp(EQ_FREQ_RANGE.0, EQ_FREQ_RANGE.1),
        gain_db: band.gain_db.clamp(-EQ_GAIN_RANGE_DB, EQ_GAIN_RANGE_DB),
        q: band.q.clamp(EQ_Q_RANGE.0, EQ_Q_RANGE.1),
        ..band
    }
}

/// One band's filter, normalized (a0 = 1).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Coefficients {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Coefficients {
    /// The band's filter at `sample_rate`; `None` when it doesn't change the
    /// sound (off, or a peak or shelf at 0 dB).
    fn of(band: &EqBand, sample_rate: f32) -> Option<Self> {
        if !band.shapes() {
            return None;
        }
        // Just under Nyquist, where the formulas still hold.
        let freq = band.freq.min(sample_rate * 0.49);
        let a = 10f32.powf(band.gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * band.q);
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            EqKind::Peak => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
            EqKind::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            EqKind::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
            EqKind::LowCut => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            EqKind::HighCut => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Some(Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 })
    }

    /// How much it raises or lowers `freq`, in dB.
    fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq / sample_rate;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let (num_re, num_im) = (self.b0 + self.b1 * cos1 + self.b2 * cos2, -(self.b1 * sin1 + self.b2 * sin2));
        let (den_re, den_im) = (1.0 + self.a1 * cos1 + self.a2 * cos2, -(self.a1 * sin1 + self.a2 * sin2));
        let power = (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im).max(1e-30);
        10.0 * power.max(1e-30).log10()
    }
}

/// How much `bands` together raise or lower `freq`, in dB: the EQ's curve.
pub fn response_db(bands: &[EqBand], freq: f32, sample_rate: f32) -> f32 {
    bands.iter().filter_map(|band| Coefficients::of(band, sample_rate)).map(|c| c.response_db(freq, sample_rate)).sum()
}

/// One band's filter for one channel, with its history (direct form I).
#[derive(Clone, Copy)]
struct Biquad {
    coefficients: Option<Coefficients>,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    const OFF: Self = Self { coefficients: None, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 };

    /// Changes the filter, keeping its history so a band moved while sound
    /// plays doesn't click; one that stops shaping forgets it.
    fn set(&mut self, coefficients: Option<Coefficients>) {
        if coefficients.is_none() {
            *self = Self::OFF;
        }
        self.coefficients = coefficients;
    }

    fn process(&mut self, x: f32) -> f32 {
        let Some(c) = self.coefficients else { return x };
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A stereo EQ: [`EQ_BANDS`] bands in series, and an on/off switch that
/// keeps them while off.
pub struct EqBands {
    sample_rate: f32,
    enabled: bool,
    bands: [EqBand; EQ_BANDS],
    left: [Biquad; EQ_BANDS],
    right: [Biquad; EQ_BANDS],
}

impl EqBands {
    pub fn new(sample_rate: f32) -> Self {
        Self::from_bands(sample_rate, true, DEFAULT_EQ_BANDS)
    }

    pub fn from_bands(sample_rate: f32, enabled: bool, bands: [EqBand; EQ_BANDS]) -> Self {
        let mut eq = Self { sample_rate, enabled, bands, left: [Biquad::OFF; EQ_BANDS], right: [Biquad::OFF; EQ_BANDS] };
        for (index, band) in bands.into_iter().enumerate() {
            eq.set_band(index, band);
        }
        eq
    }

    pub fn bands(&self) -> [EqBand; EQ_BANDS] {
        self.bands
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_band(&mut self, index: usize, band: EqBand) {
        let Some(slot) = self.bands.get_mut(index) else { return };
        *slot = clamped(band);
        let coefficients = Coefficients::of(slot, self.sample_rate);
        self.left[index].set(coefficients);
        self.right[index].set(coefficients);
    }

    /// Whether it leaves the sound as it is: off, or no band shaping it.
    pub fn is_flat(&self) -> bool {
        !self.enabled || !self.bands.iter().any(EqBand::shapes)
    }

    /// Processes one interleaved stereo frame in place, through every band.
    pub fn process(&mut self, frame: &mut [f32; 2]) {
        if !self.enabled {
            return;
        }
        for (left, right) in self.left.iter_mut().zip(self.right.iter_mut()) {
            frame[0] = left.process(frame[0]);
            frame[1] = right.process(frame[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn band(kind: EqKind, freq: f32, gain_db: f32, q: f32) -> EqBand {
        EqBand { kind, freq, gain_db, q, on: true }
    }

    /// How much `eq` raises or lowers a sine at `freq` (dB), from its RMS
    /// over whole cycles once it has settled (a sampled peak misses the
    /// true one at high frequencies).
    fn measured_db(eq: &mut EqBands, freq: f32) -> f32 {
        let mut power = 0.0f64;
        for n in 0..48_000 {
            let x = (2.0 * PI * freq * n as f32 / RATE).sin();
            let mut frame = [x, x];
            eq.process(&mut frame);
            if n >= 24_000 {
                power += f64::from(frame[0]).powi(2);
            }
        }
        let rms = (power / 24_000.0).sqrt();
        (20.0 * (rms * std::f64::consts::SQRT_2).log10()) as f32
    }

    #[test]
    fn a_flat_eq_changes_nothing() {
        let mut eq = EqBands::new(RATE);
        assert!(eq.is_flat());
        let mut frame = [0.25, -0.5];
        eq.process(&mut frame);
        assert_eq!(frame, [0.25, -0.5]);
    }

    #[test]
    fn each_kind_shapes_the_sound_as_its_curve_says() {
        for (kind, gain_db) in [(EqKind::Peak, 6.0), (EqKind::LowShelf, -6.0), (EqKind::HighShelf, 6.0), (EqKind::LowCut, 0.0), (EqKind::HighCut, 0.0)] {
            let mut bands = DEFAULT_EQ_BANDS;
            bands[5] = band(kind, 1000.0, gain_db, 0.71);
            let mut eq = EqBands::from_bands(RATE, true, bands);
            assert!(!eq.is_flat(), "{kind:?}");
            for freq in [100.0, 1000.0, 8000.0] {
                let curve = response_db(&bands, freq, RATE);
                let measured = measured_db(&mut eq, freq);
                assert!((curve - measured).abs() < 0.3, "{kind:?} at {freq} Hz: curve {curve:.2} dB, measured {measured:.2} dB");
            }
        }
    }

    #[test]
    fn bands_land_where_they_are_set() {
        let peak = [band(EqKind::Peak, 440.0, 6.0, 4.0)];
        assert!((response_db(&peak, 440.0, RATE) - 6.0).abs() < 0.05);
        assert!(response_db(&peak, 2000.0, RATE).abs() < 0.5, "a narrow peak leaves the rest alone");
        let cut = [band(EqKind::LowCut, 80.0, 0.0, 0.71)];
        assert!(response_db(&cut, 20.0, RATE) < -20.0);
        assert!(response_db(&cut, 1000.0, RATE).abs() < 0.1);
    }

    #[test]
    fn an_eq_switched_off_is_flat_but_keeps_its_bands() {
        let mut bands = DEFAULT_EQ_BANDS;
        bands[0].gain_db = 6.0;
        let mut eq = EqBands::from_bands(RATE, false, bands);
        assert!(eq.is_flat());
        assert_eq!(eq.bands()[0].gain_db, 6.0);
        eq.set_enabled(true);
        assert!(!eq.is_flat());
    }

    #[test]
    fn settings_are_kept_in_range() {
        let mut eq = EqBands::new(RATE);
        eq.set_band(0, band(EqKind::Peak, 5.0, 40.0, 0.0));
        assert_eq!(eq.bands()[0], band(EqKind::Peak, EQ_FREQ_RANGE.0, EQ_GAIN_RANGE_DB, EQ_Q_RANGE.0));
    }
}
