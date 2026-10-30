//! Clip editing: decode a saved clip's audio tracks, then write a copy with
//! each track's volume changed, a new default mix, and parts cut out. The
//! video is copied as is, never re-encoded, so a kept part can only start at
//! a keyframe (see [`ClipTimeline::kept_parts`]).

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
    #[error("not a clip Sonora can edit: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Mp4(#[from] mp4::Error),
    #[error(transparent)]
    Save(#[from] SaveError),
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
    #[error("every part of the clip is cut out")]
    NothingKept,
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
    /// The parts to keep, as `(start, end)` in 100 ns units, in order and
    /// not overlapping; `None` keeps the whole clip. Each starts at the
    /// keyframe at or before its start (see [`ClipTimeline::kept_parts`]).
    pub parts: Option<Vec<(i64, i64)>>,
}

/// When each of a clip's video frames starts and which are keyframes, from
/// the MP4's index alone (no frame data is read).
#[derive(Debug, Clone, PartialEq)]
pub struct ClipTimeline {
    /// Each frame's start, in 100 ns units from the clip's start.
    pub frame_times: Vec<i64>,
    /// Which frames playback can start from.
    pub keyframes: Vec<bool>,
    /// When the last frame ends.
    pub end: i64,
}

/// A part of a clip a save keeps: frames `first_frame..end_frame`, which
/// play from `start` to `end` (100 ns units).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptPart {
    pub first_frame: usize,
    pub end_frame: usize,
    pub start: i64,
    pub end: i64,
}

impl ClipTimeline {
    pub fn read(path: &Path) -> Result<Self, EditError> {
        let mp4 = open_mp4(path)?;
        let layout = read_layout(&mp4)?;
        let shifts = track_shifts(&mp4, &layout);
        Ok(Self::of_track(&mp4.tracks()[&layout.video], shifts.video))
    }

    /// The frames in decode order, at when they show in the clip (`shift`:
    /// where the video's own time 0 plays). With frame reordering, frames
    /// between keyframes are placed as decoded, a frame or two from when
    /// they show; keyframes, where cuts go, show exactly when placed.
    fn of_track(track: &mp4::Mp4Track, shift: i64) -> Self {
        let stbl = &track.trak.mdia.minf.stbl;
        let times = FrameTimes::of_track(track);
        let delay = times.shown_after.first().copied().unwrap_or(0) + shift;
        let frame_times: Vec<i64> = times.decode.iter().map(|decode| decode + delay).collect();
        // No sync sample table means every frame is a keyframe.
        let keyframes = match &stbl.stss {
            Some(stss) => {
                let mut keyframes = vec![false; frame_times.len()];
                for &sample in &stss.entries {
                    if let Some(keyframe) = keyframes.get_mut((sample as usize).wrapping_sub(1)) {
                        *keyframe = true;
                    }
                }
                keyframes
            }
            None => vec![true; frame_times.len()],
        };
        Self { frame_times, keyframes, end: times.end + delay }
    }

    /// What a save keeps of the parts `wanted` (`(start, end)` in 100 ns
    /// units, in order). The video is copied, not re-encoded, so a part
    /// can only start at a keyframe: each starts at the keyframe at or
    /// before its start (Sonora's clips have one every second), and ends
    /// with the last frame starting before its end. Parts that then touch
    /// or overlap are joined: the cut between them is shorter than the
    /// keyframes allow.
    pub fn kept_parts(&self, wanted: &[(i64, i64)]) -> Vec<KeptPart> {
        let mut parts: Vec<KeptPart> = Vec::new();
        for &(start, end) in wanted {
            if end <= start || self.frame_times.is_empty() {
                continue;
            }
            // The frame showing at `start`, then the keyframe it needs.
            let showing = self.frame_times.partition_point(|&t| t <= start).saturating_sub(1);
            let first_frame = (0..=showing).rev().find(|&frame| self.keyframes[frame]).unwrap_or(0);
            let end_frame = self.frame_times.partition_point(|&t| t < end);
            if end_frame <= first_frame {
                continue;
            }
            match parts.last_mut() {
                Some(last) if first_frame <= last.end_frame => last.end_frame = last.end_frame.max(end_frame),
                _ => parts.push(KeptPart { first_frame, end_frame, start: 0, end: 0 }),
            }
        }
        for part in &mut parts {
            part.start = self.frame_times[part.first_frame];
            part.end = self.frame_times.get(part.end_frame).copied().unwrap_or(self.end);
        }
        parts
    }
}

/// `audio` cut down to `parts`, joined end to end.
fn cut_audio(audio: &ClipAudio, parts: &[KeptPart]) -> ClipAudio {
    let to_frames = |hns: i64| (hns.max(0) as i128 * SAMPLE_RATE as i128 / HNS_PER_SECOND as i128) as usize;
    let frames = parts.iter().map(|part| to_frames(part.end) - to_frames(part.start)).sum();
    let tracks = audio
        .tracks
        .iter()
        .map(|track| TrackAudio {
            samples: if track.is_silent() {
                Vec::new()
            } else {
                let mut samples = Vec::with_capacity(frames * CHANNELS);
                for part in parts {
                    let (from, to) = (to_frames(part.start) * CHANNELS, to_frames(part.end) * CHANNELS);
                    let available = &track.samples[from.min(track.samples.len())..to.min(track.samples.len())];
                    samples.extend_from_slice(available);
                    // Silence where the track ended early, so later parts stay in sync.
                    samples.resize(samples.len() + (to - from - available.len()), 0);
                }
                samples
            },
        })
        .collect();
    ClipAudio { tracks, frames }
}

/// A video track's frames in decode order, from the MP4's index alone:
/// when each is decoded, and how much later it's shown (frame reordering,
/// as other recorders use; Sonora's own video has none), in 100 ns units.
pub(crate) struct FrameTimes {
    pub decode: Vec<i64>,
    pub shown_after: Vec<i64>,
    /// When the last frame's decode slot ends.
    pub end: i64,
}

impl FrameTimes {
    pub(crate) fn of_track(track: &mp4::Mp4Track) -> Self {
        let stbl = &track.trak.mdia.minf.stbl;
        let timescale = (track.timescale() as i64).max(1);
        let mut decode = Vec::new();
        let mut ticks = 0i64;
        for entry in &stbl.stts.entries {
            for _ in 0..entry.sample_count {
                decode.push(ticks_to_hns(ticks, timescale));
                ticks += entry.sample_delta as i64;
            }
        }
        let mut shown_after: Vec<i64> = match &stbl.ctts {
            Some(ctts) => ctts
                .entries
                .iter()
                .flat_map(|entry| std::iter::repeat_n(entry.sample_offset as i64, entry.sample_count as usize))
                .map(|offset| offset.signum() * ticks_to_hns(offset.abs(), timescale))
                .collect(),
            None => Vec::new(),
        };
        shown_after.resize(decode.len(), 0);
        Self { decode, shown_after, end: ticks_to_hns(ticks, timescale) }
    }
}

/// Where each track's own time 0 plays in the clip, in 100 ns units: by
/// the edit lists players follow (other recorders write them; Sonora's
/// clips have none), shifted so the earliest track starts at 0.
pub(crate) struct TrackShifts {
    pub video: i64,
    /// In layout order.
    pub audio: Vec<i64>,
}

pub(crate) fn track_shifts<R: Read + Seek>(mp4: &Mp4Reader<R>, layout: &Layout) -> TrackShifts {
    let video = &mp4.tracks()[&layout.video];
    let times = FrameTimes::of_track(video);
    let first_shown = times.decode.iter().zip(&times.shown_after).map(|(decode, after)| decode + after).min().unwrap_or(0);
    let video_shift = edit_shift(mp4, video);
    let audio: Vec<i64> = layout.audio.iter().map(|id| edit_shift(mp4, &mp4.tracks()[id])).collect();
    let origin = audio.iter().copied().chain([video_shift + first_shown]).min().unwrap_or(0);
    TrackShifts { video: video_shift - origin, audio: audio.into_iter().map(|shift| shift - origin).collect() }
}

/// Where `track`'s own time 0 plays in the movie, by its edit list: after
/// any empty edits, less the media time its first real edit starts at.
fn edit_shift<R: Read + Seek>(mp4: &Mp4Reader<R>, track: &mp4::Mp4Track) -> i64 {
    let Some(elst) = track.trak.edts.as_ref().and_then(|edts| edts.elst.as_ref()) else { return 0 };
    let movie_scale = i64::from(mp4.moov.mvhd.timescale.max(1));
    let mut lead = 0;
    for entry in &elst.entries {
        // An empty edit plays nothing for its duration (media time -1).
        if entry.media_time != u64::from(u32::MAX) && entry.media_time != u64::MAX {
            return lead - ticks_to_hns(entry.media_time as i64, (track.timescale() as i64).max(1));
        }
        lead += ticks_to_hns(entry.segment_duration as i64, movie_scale);
    }
    lead
}

/// Where a clip's tracks are, from its MP4 header. Only clips like Sonora's
/// are accepted: one H.264 track (frames may be reordered), and 48 kHz
/// stereo AAC tracks.
pub(crate) struct Layout {
    pub video: u32,
    pub audio: Vec<u32>,
}

pub(crate) fn read_layout<R: Read + Seek>(mp4: &Mp4Reader<R>) -> Result<Layout, EditError> {
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

pub(crate) fn open_mp4(path: &Path) -> Result<Mp4Reader<BufReader<File>>, EditError> {
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
    let shifts = track_shifts(&mp4, &layout);
    let _mf = MfThread::init()?;
    let mut tracks = Vec::with_capacity(layout.audio.len());
    let mut frames = 0;
    for (&track_id, &shift) in layout.audio.iter().zip(&shifts.audio) {
        let mut decoder = AacDecoder::new()?;
        let mut samples: Vec<i16> = Vec::new();
        let mut loudest = 0i16;
        // Players place decoded audio by timestamp, so do the same: that
        // keeps any decoder delay exactly as a player would hear it. Where
        // the track starts in the clip moves it all.
        let mut place = |time: i64, pcm: Bytes| {
            let at = ((time + shift) * SAMPLE_RATE as i64 + HNS_PER_SECOND / 2).div_euclid(HNS_PER_SECOND) * CHANNELS as i64;
            let pcm: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|&bytes| i16::from_le_bytes(bytes)).collect();
            // What plays before the clip starts is cut.
            let skip = (-at).clamp(0, pcm.len() as i64) as usize;
            let (at, pcm) = ((at.max(0)) as usize, &pcm[skip..]);
            if samples.len() < at + pcm.len() {
                samples.resize(at + pcm.len(), 0);
            }
            samples[at..at + pcm.len()].copy_from_slice(pcm);
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
    let shifts = track_shifts(&mp4, &layout);
    let timeline = ClipTimeline::of_track(video, shifts.video);
    let times = FrameTimes::of_track(video);
    let mut frames = Vec::new();
    let mut last_duration = 0;
    for id in 1..=mp4.sample_count(layout.video)? {
        let sample = mp4.read_sample(layout.video, id)?.ok_or(EditError::Unsupported("missing video sample"))?;
        let index = id as usize - 1;
        // Reordered frames keep their order; the copy has no edit list, so
        // its first frame shows as late as it's reordered (a frame or two).
        frames.push(VideoSample {
            pts: timeline.frame_times.get(index).copied().unwrap_or_default(),
            shown_after: times.shown_after.get(index).copied().unwrap_or_default(),
            keyframe: sample.is_sync,
            data: sample.bytes,
        });
        last_duration = sample.duration;
    }

    // Cutting: keep the parts' frames, closing the gaps between them, and
    // the audio from the same moments.
    let mut frame_duration = ticks_to_hns(last_duration as i64, timescale);
    let cut = match &edit.parts {
        Some(wanted) => {
            let parts = timeline.kept_parts(wanted);
            if parts.is_empty() {
                return Err(EditError::NothingKept);
            }
            let mut kept = Vec::new();
            let mut offset = 0;
            for part in &parts {
                for frame in &frames[part.first_frame..part.end_frame] {
                    kept.push(VideoSample { pts: frame.pts - part.start + offset, ..frame.clone() });
                }
                offset += part.end - part.start;
            }
            let (last_part, last_frame) = (parts[parts.len() - 1], &frames[parts[parts.len() - 1].end_frame - 1]);
            frame_duration = last_part.end - last_frame.pts;
            frames = kept;
            Some(cut_audio(audio, &parts))
        }
        None => None,
    };

    let _mf = MfThread::init()?;
    let mut tracks = Vec::new();
    if let Some(cut) = &cut {
        // Cut audio is encoded afresh (AAC blocks don't line up with cuts).
        let blocks = cut.frames.div_ceil(BLOCK_FRAMES as usize);
        for (index, volume) in edit.keep.iter().enumerate() {
            let Some(volume) = volume else { continue };
            let mut weights = vec![0.0; cut.tracks.len()];
            weights[index] = *volume;
            tracks.push(encode_mix(cut, &weights, blocks)?);
        }
        tracks.push(encode_mix(cut, &edit.mix, blocks)?);
    }
    for (index, &track_id) in layout.audio.iter().enumerate() {
        if cut.is_some() {
            break;
        }
        let Some(volume) = edit.keep[index] else { continue };
        let original_frames = mp4.sample_count(track_id)? as usize;
        // Copied only when it starts with the clip (the copy has no edit
        // list to move it).
        tracks.push(if (volume == 1.0 && shifts.audio[index] == 0) || audio.tracks[index].is_silent() {
            (1..=original_frames as u32)
                .map(|id| {
                    let sample = mp4.read_sample(track_id, id)?.ok_or(EditError::Unsupported("missing audio sample"))?;
                    Ok(AudioFrame { pts: block_pts(id as usize - 1), data: sample.bytes })
                })
                .collect::<Result<Vec<_>, EditError>>()?
        } else {
            let mut weights = vec![0.0; audio.tracks.len()];
            weights[index] = volume;
            // Moved later in the clip, it runs that much longer.
            encode_mix(audio, &weights, original_frames.max(audio.frames.div_ceil(BLOCK_FRAMES as usize)))?
        });
    }
    if cut.is_none() {
        let longest = layout.audio.iter().map(|&id| mp4.sample_count(id)).collect::<Result<Vec<_>, _>>()?.into_iter().max().unwrap_or(0);
        let blocks = (longest as usize).max(audio.frames.div_ceil(BLOCK_FRAMES as usize));
        tracks.push(encode_mix(audio, &edit.mix, blocks)?);
    }

    let clip = Clip {
        width,
        height,
        sps,
        pps,
        frames,
        frame_duration,
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
pub(crate) fn ticks_to_hns(ticks: i64, timescale: i64) -> i64 {
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

    /// A 10 s clip at 10 fps with a keyframe every second.
    fn timeline() -> ClipTimeline {
        let frame_times: Vec<i64> = (0..100).map(|i| i * HNS_PER_SECOND / 10).collect();
        let keyframes = (0..100).map(|i| i % 10 == 0).collect();
        ClipTimeline { frame_times, keyframes, end: 10 * HNS_PER_SECOND }
    }

    fn secs(s: f64) -> i64 {
        (s * HNS_PER_SECOND as f64).round() as i64
    }

    #[test]
    fn a_kept_part_starts_at_the_keyframe_before_it_and_ends_where_asked() {
        let parts = timeline().kept_parts(&[(secs(2.5), secs(6.25))]);
        assert_eq!(parts, [KeptPart { first_frame: 20, end_frame: 63, start: secs(2.0), end: secs(6.3) }]);
        // Starting right on a keyframe keeps exactly that.
        let parts = timeline().kept_parts(&[(secs(3.0), secs(4.0))]);
        assert_eq!((parts[0].start, parts[0].end), (secs(3.0), secs(4.0)));
        // A part running to the end ends with the clip.
        let parts = timeline().kept_parts(&[(secs(9.0), secs(10.0))]);
        assert_eq!((parts[0].end_frame, parts[0].end), (100, secs(10.0)));
    }

    #[test]
    fn a_cut_shorter_than_the_keyframes_allow_joins_its_parts() {
        // Cutting 4.2-4.6 s: the second part would start back at 4 s.
        let parts = timeline().kept_parts(&[(0, secs(4.2)), (secs(4.6), secs(10.0))]);
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].start, parts[0].end), (0, secs(10.0)));
        // Cutting 3-6 s works (the second part starts on a keyframe).
        let parts = timeline().kept_parts(&[(0, secs(3.0)), (secs(6.0), secs(10.0))]);
        assert_eq!(parts.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), [(0, secs(3.0)), (secs(6.0), secs(10.0))]);
    }

    #[test]
    fn cut_audio_joins_the_kept_moments() {
        // 1 s of audio counting up, one frame per sample pair.
        let samples: Vec<i16> = (0..48_000).flat_map(|i| [(i / 100) as i16, 0]).collect();
        let audio = ClipAudio { tracks: vec![TrackAudio { samples }, TrackAudio { samples: Vec::new() }], frames: 48_000 };
        let parts = [
            KeptPart { first_frame: 0, end_frame: 0, start: 0, end: secs(0.25) },
            KeptPart { first_frame: 0, end_frame: 0, start: secs(0.75), end: secs(1.0) },
        ];
        let cut = cut_audio(&audio, &parts);
        assert_eq!(cut.frames, 24_000);
        assert_eq!(cut.tracks[0].samples.len(), 48_000);
        assert_eq!(cut.tracks[0].frame(11_999)[0], ((11_999 / 100) as f32) / 32768.0);
        assert_eq!(cut.tracks[0].frame(12_000)[0], ((36_000 / 100) as f32) / 32768.0);
        assert!(cut.tracks[1].is_silent());
    }

    /// Cuts the middle out of a real clip (`SONORA_TEST_CLIP`), writing to
    /// a temporary folder, and checks the result's length, keyframes and
    /// audio: `cargo test -p capture-engine --lib real_clip -- --ignored`.
    #[test]
    #[ignore]
    fn cutting_a_real_clip_keeps_just_the_kept_parts() {
        let source = std::path::PathBuf::from(std::env::var("SONORA_TEST_CLIP").expect("set SONORA_TEST_CLIP"));
        let destination = std::env::temp_dir().join(format!("sonora-cut-test-{}.mp4", std::process::id()));
        let timeline = ClipTimeline::read(&source).unwrap();
        let audio = decode_clip_audio(&source).unwrap();
        let length = timeline.end;
        // Keep the first quarter and the last quarter.
        let wanted = [(0, length / 4), (length * 3 / 4, length)];
        let parts = timeline.kept_parts(&wanted);
        let expected: i64 = parts.iter().map(|p| p.end - p.start).sum();
        let tracks = audio.tracks.len();
        let edit = ClipEdit { keep: vec![Some(1.0); tracks], mix: vec![1.0; tracks], parts: Some(wanted.to_vec()) };
        save_edited_clip(&source, &destination, &audio, &edit).unwrap();

        let cut = ClipTimeline::read(&destination).unwrap();
        assert!((cut.end - expected).abs() < HNS_PER_SECOND / 50, "video is {} long, expected {}", cut.end, expected);
        assert!(cut.keyframes[0], "the cut clip starts on a keyframe");
        let cut_audio = decode_clip_audio(&destination).unwrap();
        let audio_length = cut_audio.frames as i64 * HNS_PER_SECOND / SAMPLE_RATE as i64;
        assert!((audio_length - expected).abs() < HNS_PER_SECOND / 10, "audio is {audio_length} long, video {expected}");
        assert_eq!(cut_audio.tracks.len(), tracks + 1, "every track plus the new mix");
        eprintln!(
            "cut {:.2} s down to {:.2} s ({} parts, starting at {:?} s)",
            length as f64 / 1e7,
            cut.end as f64 / 1e7,
            parts.len(),
            parts.iter().map(|p| p.start as f64 / 1e7).collect::<Vec<_>>()
        );
        std::fs::remove_file(destination).unwrap();
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
