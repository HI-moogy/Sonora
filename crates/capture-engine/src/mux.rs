use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use mp4::{AvcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};

use crate::ring::VideoSample;
use crate::SaveError;

/// 90 kHz is the conventional MP4/MPEG video clock; it represents 30 and
/// 60 fps frame durations exactly. It's also the movie timescale, because the
/// writer converts every sample's duration to it with integer division.
const VIDEO_TIMESCALE: u32 = 90_000;
const HNS_PER_SECOND: i64 = 10_000_000;

pub(crate) struct ClipVideo {
    pub width: u16,
    pub height: u16,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub frames: Vec<VideoSample>,
    /// Duration given to the final frame (100 ns units).
    pub frame_duration: i64,
}

/// Writes `video` as an MP4 at `path`. Sample durations come from the
/// frames' actual timestamps, so dropped frames don't speed the clip up.
pub(crate) fn write_clip(path: &Path, video: &ClipVideo) -> Result<(), SaveError> {
    let Some(first) = video.frames.first() else {
        return Err(SaveError::Empty);
    };
    let to_ticks = |hns: i64| (hns as i128 * VIDEO_TIMESCALE as i128 / HNS_PER_SECOND as i128) as u64;

    let file = BufWriter::new(File::create(path)?);
    let config = Mp4Config {
        major_brand: "isom".parse().expect("valid FourCC"),
        minor_version: 512,
        compatible_brands: ["isom", "iso2", "avc1", "mp41"].map(|b| b.parse().expect("valid FourCC")).to_vec(),
        timescale: VIDEO_TIMESCALE,
    };
    let mut writer = Mp4Writer::write_start(file, &config)?;
    writer.add_track(&TrackConfig {
        track_type: TrackType::Video,
        timescale: VIDEO_TIMESCALE,
        language: "und".into(),
        media_conf: MediaConfig::AvcConfig(AvcConfig {
            width: video.width,
            height: video.height,
            seq_param_set: video.sps.clone(),
            pic_param_set: video.pps.clone(),
        }),
    })?;

    for (index, frame) in video.frames.iter().enumerate() {
        let start = to_ticks(frame.pts - first.pts);
        let end = match video.frames.get(index + 1) {
            Some(next) => to_ticks(next.pts - first.pts),
            None => start + to_ticks(video.frame_duration),
        };
        writer.write_sample(
            1,
            &Mp4Sample {
                start_time: start,
                duration: end.saturating_sub(start).max(1) as u32,
                rendering_offset: 0,
                is_sync: frame.keyframe,
                bytes: frame.data.clone(),
            },
        )?;
    }

    writer.write_end()?;
    writer.into_writer().into_inner().map_err(|error| error.into_error())?;
    Ok(())
}
