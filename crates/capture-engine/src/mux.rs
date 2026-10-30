use std::fs::OpenOptions;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use mp4::{
    AacConfig, AudioObjectType, AvcConfig, ChannelConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer,
    SampleFreqIndex, TrackConfig, TrackType,
};

use crate::aac::AAC_BITRATE;
use crate::audio::{AudioFrame, BLOCK_FRAMES, SAMPLE_RATE};
use crate::ring::VideoSample;
use crate::SaveError;

/// 90 kHz is the conventional MP4/MPEG video clock; it represents 30 and
/// 60 fps frame durations exactly. It's also the movie timescale, because the
/// writer converts every sample's duration to it with integer division.
const VIDEO_TIMESCALE: u32 = 90_000;
const HNS_PER_SECOND: i64 = 10_000_000;

pub(crate) struct Clip {
    pub width: u16,
    pub height: u16,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub frames: Vec<VideoSample>,
    /// Duration given to the final frame (100 ns units).
    pub frame_duration: i64,
    /// AAC tracks in file order, each starting at the video's start. The
    /// last one (desktop audio) is the default.
    pub audio: Vec<Vec<AudioFrame>>,
}

/// Writes `video` as an MP4 at `path`: the video track, then the audio
/// tracks. Video sample durations come from the frames' actual timestamps,
/// so dropped frames don't speed the clip up.
///
/// The last audio track is the default and the only enabled one: players
/// built on ffmpeg (VLC, mpv) pick the enabled track, while Windows' own MP4
/// reader ignores that flag and picks the last audio track in the file.
pub(crate) fn write_clip(path: &Path, video: &Clip) -> Result<(), SaveError> {
    let Some(first) = video.frames.first() else {
        return Err(SaveError::Empty);
    };
    let to_ticks = |hns: i64| (hns as i128 * VIDEO_TIMESCALE as i128 / HNS_PER_SECOND as i128) as u64;

    // Read-write: the track headers are patched after the writer finishes.
    let file = BufWriter::new(OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?);
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
                rendering_offset: to_ticks(frame.shown_after.max(0)) as i32,
                is_sync: frame.keyframe,
                bytes: frame.data.clone(),
            },
        )?;
    }

    for (index, track) in video.audio.iter().enumerate() {
        let track_id = index as u32 + 2;
        writer.add_track(&TrackConfig {
            track_type: TrackType::Audio,
            timescale: SAMPLE_RATE,
            language: "und".into(),
            media_conf: MediaConfig::AacConfig(AacConfig {
                bitrate: AAC_BITRATE,
                profile: AudioObjectType::AacLowComplexity,
                freq_index: SampleFreqIndex::Freq48000,
                chan_conf: ChannelConfig::Stereo,
            }),
        })?;
        for (n, frame) in track.iter().enumerate() {
            writer.write_sample(
                track_id,
                &Mp4Sample {
                    start_time: n as u64 * BLOCK_FRAMES as u64,
                    duration: BLOCK_FRAMES as u32,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: frame.data.clone(),
                },
            )?;
        }
    }

    writer.write_end()?;
    let mut file = writer.into_writer().into_inner().map_err(|error| error.into_error())?;

    // Audio tracks share one alternate group so players treat them as
    // alternatives rather than playing all of them at once.
    let last = video.audio.len().saturating_sub(1);
    let defaults: Vec<TrackDefault> = std::iter::once(TrackDefault { enabled: true, alternate_group: 0 })
        .chain((0..video.audio.len()).map(|index| TrackDefault { enabled: index == last, alternate_group: 1 }))
        .collect();
    set_track_defaults(&mut file, &defaults)?;
    Ok(())
}

/// tkhd flags (ISO/IEC 14496-12 8.3.2).
const TRACK_ENABLED: u8 = 0x1;
const TRACK_IN_MOVIE: u8 = 0x2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackDefault {
    pub enabled: bool,
    pub alternate_group: u16,
}

/// Rewrites each track header's enabled flag and alternate group in an
/// already-written MP4, in track order. The `mp4` crate doesn't expose these
/// fields; they're fixed-size, so patching them in place is safe.
pub(crate) fn set_track_defaults<F: Read + Write + Seek>(file: &mut F, tracks: &[TrackDefault]) -> io::Result<()> {
    let end = file.seek(SeekFrom::End(0))?;
    let (start, size) = find_top_level_box(file, end, *b"moov")?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "MP4 has no moov box"))?;
    let mut moov = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut moov)?;

    let tkhds: Vec<(usize, TrackDefault)> = child_boxes(&moov, 8)
        .filter(|&(kind, _, _)| kind == *b"trak")
        .zip(tracks)
        .filter_map(|((_, trak_start, trak_end), &track)| {
            child_boxes(&moov[..trak_end], trak_start + 8)
                .find(|&(kind, _, _)| kind == *b"tkhd")
                .map(|(_, tkhd, _)| (tkhd, track))
        })
        .collect();
    for (tkhd, track) in tkhds {
        let version = moov[tkhd + 8];
        moov[tkhd + 11] = if track.enabled { TRACK_ENABLED | TRACK_IN_MOVIE } else { TRACK_IN_MOVIE };
        let group = tkhd + if version == 1 { 54 } else { 42 };
        moov[group..group + 2].copy_from_slice(&track.alternate_group.to_be_bytes());
    }

    file.seek(SeekFrom::Start(start))?;
    file.write_all(&moov)?;
    file.flush()
}

/// Finds a top-level box by type; returns its offset and total size.
fn find_top_level_box<F: Read + Seek>(file: &mut F, end: u64, kind: [u8; 4]) -> io::Result<Option<(u64, u64)>> {
    let mut offset = 0;
    while offset + 8 <= end {
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;
        let mut size = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
        if size == 1 {
            file.read_exact(&mut header[8..])?;
            size = u64::from_be_bytes(header[8..].try_into().unwrap());
        } else if size == 0 {
            size = end - offset;
        }
        if size < 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed MP4 box size"));
        }
        if header[4..8] == kind {
            return Ok(Some((offset, size)));
        }
        offset += size;
    }
    Ok(None)
}

/// Iterates (type, start, end) of the boxes laid end to end in `data`
/// starting at `offset`. Stops at the first malformed header.
fn child_boxes(data: &[u8], mut offset: usize) -> impl Iterator<Item = ([u8; 4], usize, usize)> + '_ {
    std::iter::from_fn(move || {
        let header = data.get(offset..offset + 8)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = offset.checked_add(size).filter(|&end| size >= 8 && end <= data.len())?;
        let found = (header[4..8].try_into().unwrap(), offset, end);
        offset = end;
        Some(found)
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn test_mp4() -> Cursor<Vec<u8>> {
        let config = Mp4Config {
            major_brand: "isom".parse().unwrap(),
            minor_version: 512,
            compatible_brands: vec!["isom".parse().unwrap()],
            timescale: VIDEO_TIMESCALE,
        };
        let mut writer = Mp4Writer::write_start(Cursor::new(Vec::new()), &config).unwrap();
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Video,
                timescale: VIDEO_TIMESCALE,
                language: "und".into(),
                media_conf: MediaConfig::AvcConfig(AvcConfig {
                    width: 16,
                    height: 16,
                    seq_param_set: vec![0x67, 0x64, 0x00, 0x1f],
                    pic_param_set: vec![0x68, 0xee],
                }),
            })
            .unwrap();
        for _ in 0..3 {
            writer
                .add_track(&TrackConfig {
                    track_type: TrackType::Audio,
                    timescale: SAMPLE_RATE,
                    language: "und".into(),
                    media_conf: MediaConfig::AacConfig(AacConfig::default()),
                })
                .unwrap();
        }
        for track in 1..=4 {
            let sample = Mp4Sample { start_time: 0, duration: 1024, rendering_offset: 0, is_sync: true, bytes: vec![1, 2, 3].into() };
            writer.write_sample(track, &sample).unwrap();
        }
        writer.write_end().unwrap();
        writer.into_writer()
    }

    #[test]
    fn marks_only_the_chosen_tracks_enabled_and_groups_audio() {
        let mut file = test_mp4();
        let defaults = [
            TrackDefault { enabled: true, alternate_group: 0 },
            TrackDefault { enabled: true, alternate_group: 1 },
            TrackDefault { enabled: false, alternate_group: 1 },
            TrackDefault { enabled: false, alternate_group: 1 },
        ];
        set_track_defaults(&mut file, &defaults).unwrap();

        let size = file.get_ref().len() as u64;
        file.set_position(0);
        let reader = mp4::Mp4Reader::read_header(file, size).unwrap();
        for (index, expected) in defaults.iter().enumerate() {
            let tkhd = &reader.tracks()[&(index as u32 + 1)].trak.tkhd;
            assert_eq!(tkhd.flags & 0x1 != 0, expected.enabled, "track {} enabled", index + 1);
            assert_eq!(tkhd.flags & 0x2, 0x2, "track {} stays in the movie", index + 1);
            assert_eq!(tkhd.alternate_group, expected.alternate_group, "track {} group", index + 1);
        }
    }
}
