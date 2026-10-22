//! Records the primary monitor for a few seconds and saves a clip.
//! Usage: clip_demo [seconds to record] [buffer seconds] [fps] [720|1080|1440|2160]

use std::time::{Duration, Instant};

use capture_engine::{default_clips_dir, Recorder, Resolution, VideoSettings};

fn main() {
    // The real app is per-monitor DPI aware (winit sets it); match that unless
    // SONAR_NO_DPI is set, which exercises the recorder's fallback path.
    if std::env::var_os("SONAR_NO_DPI").is_none() {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.expect("set DPI awareness");
    }
    let args: Vec<u32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let record_secs = args.first().copied().unwrap_or(12);
    let resolution = match args.get(3).copied().unwrap_or(1080) {
        720 => Resolution::P720,
        1440 => Resolution::P1440,
        2160 => Resolution::P2160,
        _ => Resolution::P1080,
    };
    let settings = VideoSettings {
        buffer_secs: args.get(1).copied().unwrap_or(10),
        fps: args.get(2).copied().unwrap_or(60),
        resolution,
    };
    println!("settings: {settings:?}, bitrate {} bps", settings.bitrate());

    let started = Instant::now();
    let recorder = Recorder::start(settings).expect("start recorder");
    println!("recorder started in {:?}", started.elapsed());

    for _ in 0..record_secs {
        std::thread::sleep(Duration::from_secs(1));
        let status = recorder.status();
        println!(
            "buffered {:>5.1}s  {:>6.1} MB  dropped {}{}",
            status.buffered().as_secs_f32(),
            status.buffered_bytes() as f64 / 1_048_576.0,
            status.dropped_frames(),
            status.error().map(|e| format!("  ERROR: {e}")).unwrap_or_default()
        );
    }

    let save_started = Instant::now();
    match recorder.save_clip(default_clips_dir()).recv().expect("save reply") {
        Ok(path) => println!(
            "saved {} ({:.1} MB) in {:?}",
            path.display(),
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1_048_576.0,
            save_started.elapsed()
        ),
        Err(error) => println!("save failed: {error}"),
    }
}
