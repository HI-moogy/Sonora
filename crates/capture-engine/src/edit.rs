//! Clip editing: decode a saved clip's audio tracks, then write a copy with
//! each track's volume changed and a new default mix. The video is copied
//! as is, never re-encoded.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use bytes::Bytes;
use mp4::{ChannelConfig, Mp4Reader, SampleFreqIndex, TrackType};

use crate::aac::{AacDecoder, AacEncoder};
use crate::audio::{AudioFrame, BLOCK_FRAMES, CHANNELS, SAMPLE_RATE};
use crate::com::MfThread;
use crate::mux::{write_clip, Clip};
use crate::ring::VideoSample;
use crate::SaveError;

const HNS_PER_SECOND: i64 = 10_000_000;
/// Samples at or below this (16-bit) count as silence: AAC leaves a little
/// noise even where nothing played.
const SILENCE: i16 = 2;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("not a clip sonar-rs can edit: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Mp4(#[from] mp4::Error),
    #[error(transparent)]
    Save(#[from] SaveError),
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
}

/// A saved clip's audio, decoded to 48 kHz stereo.
pub struct ClipAudio {
    /// Every audio track, in file order.
    pub tracks: Vec<TrackAudio>,
    /// Length in 48 kHz frames (of the longest track).
    pub frames: usize,
}

pub struct TrackAudio {
    /// Interleaved stereo; empty if the track is silent throughout.
    pub samples: Vec<i16>,
}

impl TrackAudio {
    pub fn is_silent(&self) -> bool {
        self.samples.is_empty()
    }

    /// Stereo frame `index` (silence past the end).
    pub fn frame(&self, index: usize) -> [f32; 2] {
        match self.samples.get(index * CHANNELS..index * CHANNELS + CHANNELS) {
            Some(&[left, right]) => [left as f32 / 32768.0, right as f32 / 32768.0],
            _ => [0.0; 2],
        }
    }

    /// Frames `[start, start + out.len() / 2)` scaled by `gain`, added to
    /// `out` (interleaved stereo).
    pub fn mix_into(&self, start: usize, gain: f32, out: &mut [f32]) {
        if gain == 0.0 {
            return;
        }
        let from = (start * CHANNELS).min(self.samples.len());
        let to = (from + out.len()).min(self.samples.len());
        let scale = gain / 32768.0;
        for (out, &sample) in out.iter_mut().zip(&self.samples[from..to]) {
            *out += sample as f32 * scale;
        }
    }
}

/// How to build an edited copy of a clip.
pub struct ClipEdit {
    /// Per source audio track: `Some(volume)` keeps the track, scaled by
    /// `volume`; `None` leaves it out.
    pub keep: Vec<Option<f32>>,
    /// Per source audio track: its weight in the new default track, which
    /// is written last.
    pub mix: Vec<f32>,
}

/// Where a clip's tracks are, from its MP4 header. Only the layout
/// sonar-rs writes is accepted: one H.264 track without frame reordering,
/// and 48 kHz stereo AAC tracks.
struct Layout {
    video: u32,
    audio: Vec<u32>,
}

fn read_layout<R: Read + Seek>(mp4: &Mp4Reader<R>) -> Result<Layout, EditError> {
    let mut ids: Vec<u32> = mp4.tracks().keys().copied().collect();
    ids.sort_unstable();
    let (mut video, mut audio) = (None, Vec::new());
    for id in ids {
        let track = &mp4.tracks()[&id];
        match track.track_type()? {
            TrackType::Video if video.is_none() => video = Some(id),
            TrackType::Audio => {
                let is_48k_stereo = track.sample_freq_index().is_ok_and(|f| f == SampleFreqIndex::Freq48000)
                    && track.channel_config().is_ok_and(|c| c == ChannelConfig::Stereo);
                if !is_48k_stereo {
                    return Err(EditError::Unsupported("audio isn't 48 kHz stereo"));
                }
                audio.push(id);
            }
            _ => return Err(EditError::Unsupported("unexpected track")),
        }
    }
    let video = video.ok_or(EditError::Unsupported("no video track"))?;
    if audio.is_empty() {
        return Err(EditError::Unsupported("no audio tracks"));
    }
    Ok(Layout { video, audio })
}

fn open_mp4(path: &Path) -> Result<Mp4Reader<BufReader<File>>, EditError> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    Ok(Mp4Reader::read_header(BufReader::new(file), size)?)
}

/// Decodes every audio track of the clip at `path` with Windows' own AAC
/// decoder. Tracks come from the MP4 header rather than Windows' MP4
/// reader, which lists them in an order it doesn't expose.
pub fn decode_clip_audio(path: &Path) -> Result<ClipAudio, EditError> {
    let mut mp4 = open_mp4(path)?;
    let layout = read_layout(&mp4)?;
    let _mf = MfThread::init()?;
    let mut tracks = Vec::with_capacity(layout.audio.len());
    let mut frames = 0;
    for &track_id in &layout.audio {
        let mut decoder = AacDecoder::new()?;
        let mut samples: Vec<i16> = Vec::new();
        let mut loudest = 0i16;
        // Players place decoded audio by timestamp, so do the same: that
        // keeps any decoder delay exactly as a player would hear it.
        let mut place = |time: i64, pcm: Bytes| {
            let at = (time * SAMPLE_RATE as i64 + HNS_PER_SECOND / 2).div_euclid(HNS_PER_SECOND).max(0) as usize * CHANNELS;
            let pcm: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|&bytes| i16::from_le_bytes(bytes)).collect();
            if samples.len() < at + pcm.len() {
                samples.resize(at + pcm.len(), 0);
            }
            samples[at..at + pcm.len()].copy_from_slice(&pcm);
            loudest = pcm.iter().fold(loudest, |loudest, s| loudest.max(s.saturating_abs()));
        };
        for id in 1..=mp4.sample_count(track_id)? {
            let frame = mp4.read_sample(track_id, id)?.ok_or(EditError::Unsupported("missing audio sample"))?;
            decoder.decode(&frame.bytes, block_pts(id as usize - 1), &mut place)?;
        }
        decoder.drain(&mut place)?;
        // Measured before a silent track's samples are dropped, so a clip
        // that's silent throughout still has its length.
        frames = frames.max(samples.len() / CHANNELS);
        tracks.push(TrackAudio { samples: if loudest <= SILENCE { Vec::new() } else { samples } });
    }
    Ok(ClipAudio { tracks, frames })
}

/// Writes an edited copy of the clip at `source` to `destination` (which
/// may be `source` itself): the video copied as is, the audio tracks `edit`
/// keeps, then the new mix as the default track. A kept track at 100% is
/// copied rather than re-encoded. `audio` must be `source`'s decoded audio.
pub fn save_edited_clip(source: &Path, destination: &Path, audio: &ClipAudio, edit: &ClipEdit) -> Result<(), EditError> {
    let mut mp4 = open_mp4(source)?;
    let layout = read_layout(&mp4)?;
    if layout.audio.len() != audio.tracks.len() || edit.keep.len() != audio.tracks.len() || edit.mix.len() != audio.tracks.len() {
        return Err(EditError::Unsupported("the edit doesn't match the clip's tracks"));
    }

    let video = &mp4.tracks()[&layout.video];
    let (width, height) = (video.width(), video.height());
    let (sps, pps) = (video.sequence_parameter_set()?.to_vec(), video.picture_parameter_set()?.to_vec());
    let timescale = video.timescale() as i64;
    let mut frames = Vec::new();
    let mut last_duration = 0;
    for id in 1..=mp4.sample_count(layout.video)? {
        let sample = mp4.read_sample(layout.video, id)?.ok_or(EditError::Unsupported("missing video sample"))?;
        if sample.rendering_offset != 0 {
            return Err(EditError::Unsupported("video uses frame reordering"));
        }
        frames.push(VideoSample {
            pts: ticks_to_hns(sample.start_time as i64, timescale),
            keyframe: sample.is_sync,
            data: sample.bytes,
        });
        last_duration = sample.duration;
    }

    let _mf = MfThread::init()?;
    let mut tracks = Vec::new();
    for (index, &track_id) in layout.audio.iter().enumerate() {
        let Some(volume) = edit.keep[index] else { continue };
        let original_frames = mp4.sample_count(track_id)? as usize;
        tracks.push(if volume == 1.0 || audio.tracks[index].is_silent() {
            (1..=original_frames as u32)
                .map(|id| {
                    let sample = mp4.read_sample(track_id, id)?.ok_or(EditError::Unsupported("missing audio sample"))?;
                    Ok(AudioFrame { pts: block_pts(id as usize - 1), data: sample.bytes })
                })
                .collect::<Result<Vec<_>, EditError>>()?
        } else {
            let mut weights = vec![0.0; audio.tracks.len()];
            weights[index] = volume;
            encode_mix(audio, &weights, original_frames)?
        });
    }
    let longest = layout.audio.iter().map(|&id| mp4.sample_count(id)).collect::<Result<Vec<_>, _>>()?.into_iter().max().unwrap_or(0);
    tracks.push(encode_mix(audio, &edit.mix, longest as usize)?);

    let clip = Clip {
        width,
        height,
        sps,
        pps,
        frames,
        frame_duration: ticks_to_hns(last_duration as i64, timescale),
        audio: tracks,
    };
    // Written under a temporary name first, so a failed save never leaves
    // a broken clip, or replaces the original with one.
    let partial = destination.with_extension("mp4.partial");
    if let Err(error) = write_clip(&partial, &clip) {
        let _ = std::fs::remove_file(&partial);
        return Err(error.into());
    }
    drop(mp4);
    std::fs::rename(&partial, destination).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    Ok(())
}

/// Encodes `Σ weights[i] × track i` as `blocks` AAC frames.
fn encode_mix(audio: &ClipAudio, weights: &[f32], blocks: usize) -> Result<Vec<AudioFrame>, EditError> {
    let mut encoder = AacEncoder::new()?;
    let mut out = Vec::with_capacity(blocks + 2);
    let mut push = |_pts, data| out.push(AudioFrame { pts: block_pts(out.len()), data });
    let mut block = vec![0.0f32; BLOCK_FRAMES as usize * CHANNELS];
    let mut pcm = vec![0i16; block.len()];
    for index in 0..blocks {
        block.fill(0.0);
        for (track, &weight) in audio.tracks.iter().zip(weights) {
            track.mix_into(index * BLOCK_FRAMES as usize, weight, &mut block);
        }
        for (out, &sample) in pcm.iter_mut().zip(&block) {
            *out = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
        encoder.encode(&pcm, block_pts(index), &mut push)?;
    }
    encoder.drain(&mut push)?;
    out.truncate(blocks);
    Ok(out)
}

fn block_pts(index: usize) -> i64 {
    index as i64 * BLOCK_FRAMES * HNS_PER_SECOND / SAMPLE_RATE as i64
}

/// Rounds up, so converting back to `timescale` ticks (rounding down, as
/// the MP4 writer does) gives exactly `ticks` again.
fn ticks_to_hns(ticks: i64, timescale: i64) -> i64 {
    (ticks * HNS_PER_SECOND + timescale - 1) / timescale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversion_round_trips_through_the_writer() {
        let to_ticks = |hns: i64| (hns as i128 * 90_000 / HNS_PER_SECOND as i128) as i64;
        for ticks in [0, 1, 1499, 1500, 1501, 3000, 89_999, 90_000, 5_400_123] {
            assert_eq!(to_ticks(ticks_to_hns(ticks, 90_000)), ticks);
        }
    }

    #[test]
    fn mixing_scales_and_sums_and_pads_past_the_end() {
        let track = TrackAudio { samples: vec![16384, -16384, 8192, 8192] };
        let mut out = vec![0.25f32; 6];
        track.mix_into(0, 0.5, &mut out);
        assert_eq!(out, [0.5, 0.0, 0.375, 0.375, 0.25, 0.25]);
        assert_eq!(track.frame(1), [0.25, 0.25]);
        assert_eq!(track.frame(2), [0.0, 0.0]);
    }
}
