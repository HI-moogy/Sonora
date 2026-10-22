use std::sync::atomic::{AtomicU32, Ordering};

/// A lock-free f32 for values shared between the mixer's audio threads and
/// the UI thread (gain, mute flags encoded as 0.0/1.0, meter levels), where
/// a brief staleness is harmless and a mutex would be unnecessary overhead
/// on the audio callback path.
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}
