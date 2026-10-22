use std::f32::consts::PI;

pub const EQ_BAND_COUNT: usize = 5;
pub const EQ_BAND_FREQUENCIES: [f32; EQ_BAND_COUNT] = [100.0, 350.0, 1000.0, 3500.0, 10_000.0];
pub const EQ_GAIN_RANGE_DB: f32 = 12.0;

/// A single RBJ-cookbook peaking biquad. At 0dB gain its transfer function
/// is the exact identity (b_i/a0 == a_i/a0 for i=1,2, b0/a0==1), so bands
/// left at their default gain add no coloration.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Recomputes coefficients for a peaking EQ band; filter state (the
    /// x1/x2/y1/y2 history) is left untouched so changing gain mid-stream
    /// doesn't click.
    fn set_peaking(&mut self, sample_rate: f32, freq: f32, q: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A stereo 5-band peaking EQ, bands fixed at [`EQ_BAND_FREQUENCIES`],
/// gains individually adjustable within +/-[`EQ_GAIN_RANGE_DB`].
pub struct EqBands {
    sample_rate: f32,
    gains_db: [f32; EQ_BAND_COUNT],
    left: [Biquad; EQ_BAND_COUNT],
    right: [Biquad; EQ_BAND_COUNT],
}

impl EqBands {
    pub fn new(sample_rate: f32) -> Self {
        Self::from_gains(sample_rate, [0.0; EQ_BAND_COUNT])
    }

    pub fn from_gains(sample_rate: f32, gains_db: [f32; EQ_BAND_COUNT]) -> Self {
        let mut eq = Self {
            sample_rate,
            gains_db,
            left: [Biquad::new(); EQ_BAND_COUNT],
            right: [Biquad::new(); EQ_BAND_COUNT],
        };
        for band in 0..EQ_BAND_COUNT {
            eq.recompute_band(band);
        }
        eq
    }

    pub fn gains(&self) -> [f32; EQ_BAND_COUNT] {
        self.gains_db
    }

    pub fn set_band(&mut self, band: usize, gain_db: f32) {
        if band >= EQ_BAND_COUNT {
            return;
        }
        self.gains_db[band] = gain_db.clamp(-EQ_GAIN_RANGE_DB, EQ_GAIN_RANGE_DB);
        self.recompute_band(band);
    }

    fn recompute_band(&mut self, band: usize) {
        let freq = EQ_BAND_FREQUENCIES[band];
        let q = 1.0;
        let gain_db = self.gains_db[band];
        self.left[band].set_peaking(self.sample_rate, freq, q, gain_db);
        self.right[band].set_peaking(self.sample_rate, freq, q, gain_db);
    }

    /// Processes one interleaved stereo frame in place, through all bands
    /// in series.
    pub fn process(&mut self, frame: &mut [f32; 2]) {
        let mut l = frame[0];
        let mut r = frame[1];
        for band in 0..EQ_BAND_COUNT {
            l = self.left[band].process(l);
            r = self.right[band].process(r);
        }
        frame[0] = l;
        frame[1] = r;
    }
}
