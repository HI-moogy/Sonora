//! Confirmation sounds for saving clips: a short rising chime when a clip
//! is saved, a falling one when it fails. Synthesized once, then played
//! asynchronously from memory.

use std::f32::consts::TAU;
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const SAMPLE_RATE: u32 = 48_000;

pub fn clip_saved() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    // E6 then A6: bright and quick, easy to pick out over game audio.
    play(WAV.get_or_init(|| chime(&[(1318.5, 0.08), (1760.0, 0.18)])));
}

pub fn clip_failed() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    // A4 then E-flat 4: lower and falling, so it can't be mistaken for success.
    play(WAV.get_or_init(|| chime(&[(440.0, 0.14), (311.1, 0.24)])));
}

fn play(wav: &'static [u8]) {
    // SND_MEMORY reads the WAV from this pointer while it plays, which is
    // why the data lives in a static.
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// Plays `notes` (frequency in Hz, length in seconds) back to back, each a
/// soft bell: a sine plus a quiet octave, quick attack, exponential decay.
fn chime(notes: &[(f32, f32)]) -> Vec<u8> {
    const PEAK: f32 = 0.35;
    let mut samples = Vec::new();
    for &(frequency, seconds) in notes {
        let count = (seconds * SAMPLE_RATE as f32) as usize;
        for i in 0..count {
            let t = i as f32 / SAMPLE_RATE as f32;
            let attack = (t / 0.004).min(1.0);
            let decay = (-t / (seconds * 0.35)).exp();
            let tone = ((TAU * frequency * t).sin() + 0.25 * (TAU * 2.0 * frequency * t).sin()) / 1.25;
            samples.push((tone * attack * decay * PEAK * i16::MAX as f32) as i16);
        }
    }
    wav(&samples)
}

/// A mono 16-bit PCM WAV file.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chime_is_a_well_formed_wav_of_the_right_length() {
        let bytes = chime(&[(1000.0, 0.1), (2000.0, 0.2)]);
        let samples = (0.3 * SAMPLE_RATE as f32) as usize;
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize, samples * 2);
        assert_eq!(bytes.len(), 44 + samples * 2);
    }

    #[test]
    fn chime_stays_within_range_and_is_audible() {
        let bytes = chime(&[(1318.5, 0.08)]);
        let peak = bytes[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs()).max().unwrap();
        assert!(peak > 5_000 && peak < i16::MAX as u16 / 2, "peak {peak}");
    }
}
