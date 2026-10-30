//! Makes the copies of a clip Sonora's Share to Discord makes, and says what
//! each came out as: its size and the plan it followed.
//! Usage: share_clip <clip.mp4> <folder> <limit in MB>

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use capture_engine::{plan_share, share_clip, ShareSource};

fn main() {
    let mut args = std::env::args().skip(1);
    let (clip, folder, limit_mb) = (args.next().expect("clip"), args.next().expect("folder"), args.next().expect("limit"));
    let limit = limit_mb.parse::<u64>().expect("limit in MB") * 1_000_000;
    let source = ShareSource::read(Path::new(&clip)).expect("a clip Sonora can read");
    println!("{source:?}");
    println!("{:?}", plan_share(&source, limit));
    let tracks = capture_engine::decode_clip_audio(Path::new(&clip)).expect("audio").tracks.len();
    // As Sonora shares: Desktop and Mic of a Sonora clip, an edited clip's
    // mix, or every track of another recorder's.
    let mix: Vec<f32> = match tracks {
        6 => vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0],
        7 => vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        n => vec![1.0; n],
    };
    let started = Instant::now();
    let copies = share_clip(Path::new(&clip), Path::new(&folder), limit, &mix, &|_| {}, &AtomicBool::new(false));
    println!("took {:.1} s", started.elapsed().as_secs_f32());
    for copy in copies.expect("shared") {
        let size = std::fs::metadata(&copy).map_or(0, |meta| meta.len());
        println!("{} {:.2} MB", copy.display(), size as f64 / 1e6);
    }
}
