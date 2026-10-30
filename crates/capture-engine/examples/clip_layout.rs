//! Prints a clip's MP4 layout: each track's kind, size, rate and length,
//! and for video its first frames' keyframe flags and reordering offsets.
//! Usage: clip_layout <clip.mp4>

use std::fs::File;
use std::io::BufReader;

fn main() {
    let path = std::env::args().nth(1).expect("usage: clip_layout <clip.mp4>");
    let file = File::open(&path).expect("open");
    let size = file.metadata().expect("size").len();
    let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size).expect("an MP4");
    let mut ids: Vec<u32> = mp4.tracks().keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let track = &mp4.tracks()[&id];
        let kind = track.track_type().map(|kind| format!("{kind:?}")).unwrap_or_else(|e| format!("? ({e})"));
        println!(
            "track {id}: {kind}, {} samples over {:.2} s, timescale {}, {}x{}, audio {:?} {:?}, edit list: {}",
            track.sample_count(),
            track.duration().as_secs_f64(),
            track.timescale(),
            track.width(),
            track.height(),
            track.sample_freq_index().ok(),
            track.channel_config().ok(),
            track.trak.edts.as_ref().map_or("none".to_string(), |edts| format!("{:?}", edts.elst.as_ref().map(|elst| &elst.entries))),
        );
        if kind == "Video" {
            let samples: Vec<String> = (1..=track.sample_count().min(12))
                .filter_map(|n| mp4.read_sample(id, n).ok().flatten())
                .map(|sample| format!("{}{}+{}", if sample.is_sync { "K" } else { "" }, sample.start_time, sample.rendering_offset))
                .collect();
            println!("  first frames (decode time + reorder offset): {}", samples.join(" "));
        }
    }
}
