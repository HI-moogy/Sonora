//! Measures what routing does to sound quality. Captures an app playing a
//! pure tone twice: as-is, then turned down to −80 dB and boosted back up
//! exactly as the mixer does. For each, fits an ideal sine at the tone's
//! frequency and reports the level and signal-to-noise ratio (everything
//! that isn't the sine counts as noise or distortion).
//! Usage: routing_quality <pid> <tone frequency in Hz>

use std::collections::HashSet;
use std::f64::consts::TAU;
use std::time::{Duration, Instant};

use audio_engine::loopback::{activate_process_loopback_client, standard_format};
use audio_engine::session_volume::{self, ROUTED_BOOST};
use audio_engine::ComGuard;
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};

const SAMPLE_RATE: f64 = 48_000.0;

/// Left-channel samples of `pid`'s audio for `seconds`.
fn capture(pid: u32, seconds: f32) -> windows::core::Result<Vec<f32>> {
    let client = activate_process_loopback_client(pid)?;
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &standard_format(),
            None,
        )?;
        client.Start()?;
    }
    let capture: IAudioCaptureClient = unsafe { client.GetService()? };
    let mut left = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f32(seconds) {
        std::thread::sleep(Duration::from_millis(10));
        while unsafe { capture.GetNextPacketSize()? } > 0 {
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
            let samples =
                unsafe { std::slice::from_raw_parts(data as *const f32, frames as usize * 2) };
            left.extend(samples.iter().step_by(2));
            unsafe { capture.ReleaseBuffer(frames)? };
        }
    }
    Ok(left)
}

/// Least-squares fit of `a·cos + b·sin + dc` at `frequency`; returns the
/// sine's peak amplitude and the signal-to-noise ratio in dB.
fn sine_fit(samples: &[f32], frequency: f64) -> (f64, f64) {
    let w = TAU * frequency / SAMPLE_RATE;
    let n = samples.len() as f64;
    let dc = samples.iter().map(|&s| s as f64).sum::<f64>() / n;
    let (mut a, mut b) = (0.0, 0.0);
    for (i, &s) in samples.iter().enumerate() {
        let x = s as f64 - dc;
        a += x * (w * i as f64).cos();
        b += x * (w * i as f64).sin();
    }
    let (a, b) = (2.0 * a / n, 2.0 * b / n);
    let (mut signal, mut noise) = (0.0, 0.0);
    for (i, &s) in samples.iter().enumerate() {
        let fit = a * (w * i as f64).cos() + b * (w * i as f64).sin();
        signal += fit * fit;
        noise += (s as f64 - dc - fit).powi(2);
    }
    (
        (a * a + b * b).sqrt(),
        10.0 * (signal / noise.max(1e-300)).log10(),
    )
}

/// One second from the middle of the capture, clear of start-up effects.
fn middle(samples: &[f32]) -> &[f32] {
    let len = SAMPLE_RATE as usize;
    let start = samples.len().saturating_sub(len) / 2;
    &samples[start..(start + len).min(samples.len())]
}

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().expect("pid").parse().expect("pid is a number");
    let frequency: f64 = args
        .next()
        .expect("tone frequency")
        .parse()
        .expect("frequency is a number");

    let direct = capture(pid, 2.0)?;
    let (amplitude, snr) = sine_fit(middle(&direct), frequency);
    println!("as-is:                 amplitude {amplitude:.6}, SNR {snr:.1} dB");

    let pids = HashSet::from([pid]);
    let routed = session_volume::route(&pids);
    std::thread::sleep(Duration::from_millis(300));
    let quiet = capture(pid, 2.0)?;
    session_volume::unroute(&pids);
    let boosted: Vec<f32> = quiet.iter().map(|&s| s * ROUTED_BOOST).collect();
    let (amplitude, snr) = sine_fit(middle(&boosted), frequency);
    println!("turned down + boosted: amplitude {amplitude:.6}, SNR {snr:.1} dB (all sessions turned down: {routed})");
    Ok(())
}
