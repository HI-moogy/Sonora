//! Writes an edited copy of a clip and checks it against the original.
//! Usage: edit_clip <source.mp4> <destination.mp4> <volumes> [mix weights]
//! Volumes and weights are comma-separated, one per audio track in file
//! order; a volume of `-` drops the track. Weights default to the volumes.
//!
//! Prints each track's level, and for each kept, re-encoded track its lag
//! against the original (it should be 0).

use std::path::Path;
use std::time::Instant;

use capture_engine::{decode_clip_audio, save_edited_clip, ClipAudio, ClipEdit, TrackAudio};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source, destination, volumes, rest @ ..] = &args[..] else {
        panic!("usage: edit_clip <source.mp4> <destination.mp4> <volumes> [mix weights]");
    };
    let keep: Vec<Option<f32>> = volumes.split(',').map(|v| v.parse().ok()).collect();
    let mix: Vec<f32> = match rest.first() {
        Some(weights) => weights.split(',').map(|w| w.parse().expect("weight")).collect(),
        None => keep.iter().map(|v| v.unwrap_or(0.0)).collect(),
    };

    let started = Instant::now();
    let audio = decode_clip_audio(Path::new(source)).expect("decode source");
    println!("decoded {} tracks, {:.3}s, in {:?}", audio.tracks.len(), audio.frames as f64 / 48_000.0, started.elapsed());
    report(&audio);

    let started = Instant::now();
    save_edited_clip(Path::new(source), Path::new(destination), &audio, &ClipEdit { keep: keep.clone(), mix, parts: None })
        .expect("save edited clip");
    println!("saved in {:?}", started.elapsed());

    let edited = decode_clip_audio(Path::new(destination)).expect("decode edited clip");
    println!("edited clip: {} tracks, {:.3}s", edited.tracks.len(), edited.frames as f64 / 48_000.0);
    report(&edited);
    let kept = keep.iter().enumerate().filter_map(|(index, volume)| volume.map(|v| (index, v)));
    for (out_index, (index, volume)) in kept.enumerate() {
        let (original, copy) = (&audio.tracks[index], &edited.tracks[out_index]);
        if original.is_silent() || copy.is_silent() {
            continue;
        }
        let lag = best_lag(original, copy);
        let gain = rms(copy) / rms(original).max(1e-9);
        println!("track {index}: volume {volume} → measured gain {gain:.3}, lag {lag} samples");
    }
}

fn report(audio: &ClipAudio) {
    for (index, track) in audio.tracks.iter().enumerate() {
        if track.is_silent() {
            println!("  track {index}: silent");
        } else {
            println!("  track {index}: RMS {:.4}", rms(track));
        }
    }
}

fn rms(track: &TrackAudio) -> f64 {
    let sum: f64 = track.samples.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    (sum / track.samples.len().max(1) as f64).sqrt()
}

/// The shift of `b` against `a` (in frames, left channel) that correlates best.
fn best_lag(a: &TrackAudio, b: &TrackAudio) -> i64 {
    let left = |t: &TrackAudio| t.samples.iter().step_by(2).map(|&s| s as f64).collect::<Vec<_>>();
    let (a, b) = (left(a), left(b));
    let n = a.len().min(b.len()).min(48_000 * 10);
    (-4096i64..=4096)
        .max_by(|&x, &y| {
            let score = |lag: i64| -> f64 {
                (0..n as i64)
                    .filter_map(|i| Some(a[i as usize] * *b.get(usize::try_from(i + lag).ok()?)?))
                    .sum()
            };
            score(x).total_cmp(&score(y))
        })
        .unwrap()
}
