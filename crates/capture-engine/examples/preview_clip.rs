//! Decodes a clip's whole video the way the in-app preview does, as fast
//! as it can, then seeks to the middle; reports frame size and timing.
//! Usage: preview_clip <path.mp4> [max width]

use std::path::Path;
use std::time::{Duration, Instant};

use capture_engine::VideoPreview;

fn main() {
    let path = std::env::args().nth(1).expect("usage: preview_clip <path.mp4> [max width]");
    let max_width = std::env::args().nth(2).and_then(|w| w.parse().ok()).unwrap_or(960);
    let started = Instant::now();
    let mut preview = VideoPreview::open(Path::new(&path), max_width, || {});
    let (mut frames, mut last_pts, mut size) = (0, 0, (0, 0));
    let mut first_at = None;
    let mut idle = Instant::now();
    while idle.elapsed() < Duration::from_secs(1) {
        match preview.next_frame() {
            Some(Ok(frame)) => {
                first_at.get_or_insert(started.elapsed());
                frames += 1;
                last_pts = frame.pts;
                size = (frame.width, frame.height);
                idle = Instant::now();
            }
            Some(Err(error)) => panic!("decode failed: {error}"),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let elapsed = started.elapsed() - Duration::from_secs(1);
    println!(
        "{frames} frames at {}x{}, last at {:.3}s; first after {:?}, all in {elapsed:?} ({:.0} fps)",
        size.0,
        size.1,
        last_pts as f64 / 1e7,
        first_at.unwrap_or_default(),
        frames as f64 / elapsed.as_secs_f64()
    );

    let target = last_pts / 2;
    let seek_started = Instant::now();
    let seek = preview.seek(target);
    loop {
        match preview.next_frame() {
            Some(Ok(frame)) if frame.seek == seek => {
                println!("seek to {:.3}s landed on {:.3}s after {:?}", target as f64 / 1e7, frame.pts as f64 / 1e7, seek_started.elapsed());
                break;
            }
            Some(Err(error)) => panic!("decode failed: {error}"),
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}
