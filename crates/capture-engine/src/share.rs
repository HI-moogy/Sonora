//! Copies of a clip to share where uploads have a size limit (Discord's):
//! each copy fits under the size you choose, with one audio track (players
//! there play only one). A clip that fits as it is keeps its video and only
//! has its audio mixed; one that doesn't is encoded again, smaller, and split
//! into parts when one file couldn't hold it at a watchable quality.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_High, IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MFAudioFormat_AAC,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_DISABLE_THROTTLING,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
};

use crate::audio::{CHANNELS, SAMPLE_RATE};
use crate::com::MfThread;
use crate::edit::{decode_clip_audio, open_mp4, read_layout, save_edited_clip, ClipAudio, ClipEdit, EditError};

const HNS_PER_SECOND: i64 = 10_000_000;
const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
const ALL_STREAMS: u32 = 0xFFFF_FFFE;
/// The audio of an encoded copy.
const AUDIO_BITRATE: u32 = 128_000;
/// Copies aim this far under the limit: the MP4's own bytes, and encoders
/// overshooting a little.
const SIZE_MARGIN: f64 = 0.92;
/// Below this, game footage turns to mush even small: a part gets at least
/// this, and the clip is split into more parts instead.
const MIN_VIDEO_BITRATE: u32 = 1_500_000;
/// More than this buys nothing a viewer would see.
const MAX_VIDEO_BITRATE: u32 = 12_000_000;
/// How often a copy that came out too big is encoded again, smaller.
const RETRIES: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("stopped")]
    Cancelled,
    #[error("couldn't get a part under the size limit")]
    TooBig,
}

/// What a clip is, for planning its copies.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareSource {
    /// In 100 ns units.
    pub duration: i64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_bytes: u64,
    pub audio_tracks: usize,
}

impl ShareSource {
    pub fn read(path: &Path) -> Result<Self, EditError> {
        let mp4 = open_mp4(path)?;
        let layout = read_layout(&mp4)?;
        let video = &mp4.tracks()[&layout.video];
        let duration = video.duration().as_micros() as i64 * 10;
        let frames = video.sample_count() as f64;
        Ok(Self {
            duration,
            width: u32::from(video.width()),
            height: u32::from(video.height()),
            fps: if duration > 0 { frames * HNS_PER_SECOND as f64 / duration as f64 } else { 30.0 },
            video_bytes: video.trak.mdia.minf.stbl.stsz.sample_sizes.iter().map(|&size| u64::from(size)).sum(),
            audio_tracks: layout.audio.len(),
        })
    }
}

/// How a clip is made into copies under a size limit.
#[derive(Debug, Clone, PartialEq)]
pub enum SharePlan {
    /// It fits: the video as it is, the audio mixed to one track.
    AsIs,
    Encode(EncodePlan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodePlan {
    /// Each copy's `(start, end)`, in 100 ns units.
    pub parts: Vec<(i64, i64)>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u32,
}

/// Plans copies of `source` that each fit in `limit` bytes: as it is if it
/// fits; otherwise as few parts as keep each at a watchable bitrate, sized
/// down to suit the bitrate each gets.
pub fn plan_share(source: &ShareSource, limit: u64) -> SharePlan {
    let seconds = source.duration as f64 / HNS_PER_SECOND as f64;
    let audio_bytes = f64::from(crate::aac::AAC_BITRATE) / 8.0 * seconds;
    if (source.video_bytes as f64 + audio_bytes) <= limit as f64 * SIZE_MARGIN {
        return SharePlan::AsIs;
    }
    SharePlan::Encode(encode_plan(source, limit, 1.0))
}

/// Parts and quality for copies that fit in `limit` bytes, at `scale` times
/// the bitrate that would just fill them (lower after a copy came out too
/// big).
fn encode_plan(source: &ShareSource, limit: u64, scale: f64) -> EncodePlan {
    let seconds = (source.duration as f64 / HNS_PER_SECOND as f64).max(0.1);
    let budget = limit as f64 * 8.0 * SIZE_MARGIN * scale;
    let per_part = f64::from(MIN_VIDEO_BITRATE + AUDIO_BITRATE) * seconds;
    let count = (per_part / budget).ceil().max(1.0) as i64;
    let part_seconds = seconds / count as f64;
    let video_bitrate = ((budget / part_seconds) - f64::from(AUDIO_BITRATE)).clamp(1.0, f64::from(MAX_VIDEO_BITRATE)) as u32;

    // Fewer pixels and frames for fewer bits, never more than the clip has.
    // "720p" is the short side, so a portrait clip stays as sharp.
    let (max_short_side, max_fps) = match video_bitrate {
        9_000_000.. => (1080, 60),
        5_000_000.. => (720, 60),
        2_000_000.. => (720, 30),
        _ => (540, 30),
    };
    let short_side = source.width.min(source.height).max(1);
    let scaled = |side: u32| ((u64::from(side) * u64::from(short_side.min(max_short_side)) / u64::from(short_side)) as u32).max(2) & !1;
    let (width, height) = (scaled(source.width), scaled(source.height));
    let fps = (source.fps.round() as u32).clamp(1, max_fps);
    let parts = (0..count)
        .map(|part| (source.duration * part / count, source.duration * (part + 1) / count))
        .collect();
    EncodePlan { parts, width, height, fps, video_bitrate }
}

/// Makes the copies of the clip at `path` in `folder`, named after it, each
/// under `limit` bytes, with one audio track mixed from the clip's tracks by
/// `mix` (a weight per track). Copies an earlier share of the clip left are
/// replaced. `progress` hears how far along it is (0 to 1). Returns the
/// copies, in order.
pub fn share_clip(
    path: &Path,
    folder: &Path,
    limit: u64,
    mix: &[f32],
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, ShareError> {
    let _mf = MfThread::init()?;
    let source = ShareSource::read(path)?;
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "Clip".into());
    std::fs::create_dir_all(folder)?;
    remove_earlier_copies(folder, &stem);
    let audio = decode_clip_audio(path)?;
    if mix.len() != audio.tracks.len() {
        return Err(EditError::Unsupported("the mix doesn't match the clip's tracks").into());
    }

    if plan_share(&source, limit) == SharePlan::AsIs {
        let copy = folder.join(format!("{stem} (Discord).mp4"));
        let edit = ClipEdit { keep: vec![None; mix.len()], mix: mix.to_vec(), parts: None };
        save_edited_clip(path, &copy, &audio, &edit)?;
        if std::fs::metadata(&copy)?.len() <= limit {
            progress(1.0);
            return Ok(vec![copy]);
        }
        std::fs::remove_file(&copy)?;
    }

    let mixed = mix_audio(&audio, mix);
    let mut scale = 1.0;
    for _ in 0..=RETRIES {
        let plan = encode_plan(&source, limit, scale);
        let copies = encode_parts(path, folder, &stem, &plan, &mixed, progress, cancel);
        let copies = match copies {
            Ok(copies) => copies,
            Err(error) => {
                remove_earlier_copies(folder, &stem);
                return Err(error);
            }
        };
        let largest = copies.iter().filter_map(|copy| std::fs::metadata(copy).ok()).map(|meta| meta.len()).max().unwrap_or(0);
        if largest <= limit {
            return Ok(copies);
        }
        // Came out too big: again, smaller by as much as it was over.
        remove_earlier_copies(folder, &stem);
        scale *= limit as f64 * SIZE_MARGIN / largest as f64;
    }
    Err(ShareError::TooBig)
}

/// Deletes what an earlier share of the clip named `stem` left in `folder`.
fn remove_earlier_copies(folder: &Path, stem: &str) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let prefix = format!("{stem} (Discord");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".mp4") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The clip's tracks mixed by `mix`, interleaved stereo.
fn mix_audio(audio: &ClipAudio, mix: &[f32]) -> Vec<i16> {
    let mut mixed = vec![0.0f32; audio.frames * CHANNELS];
    for (track, &weight) in audio.tracks.iter().zip(mix) {
        track.mix_into(0, weight, &mut mixed);
    }
    mixed.iter().map(|&sample| (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).collect()
}

fn part_name(stem: &str, part: usize, count: usize) -> String {
    if count == 1 {
        format!("{stem} (Discord).mp4")
    } else {
        format!("{stem} (Discord {} of {count}).mp4", part + 1)
    }
}

/// Encodes the parts `plan` asks for, in one pass through the clip's video.
fn encode_parts(
    path: &Path,
    folder: &Path,
    stem: &str,
    plan: &EncodePlan,
    audio: &[i16],
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, ShareError> {
    let reader = open_video(path, plan)?;
    let frame_duration = HNS_PER_SECOND / i64::from(plan.fps);
    let total = plan.parts.last().map_or(1, |&(_, end)| end).max(1);
    let mut copies = Vec::new();
    let mut part: Option<PartWriter> = None;
    let mut next_frame = 0i64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ShareError::Cancelled);
        }
        let (mut flags, mut time, mut sample): (u32, i64, Option<IMFSample>) = (0, 0, None);
        unsafe { reader.ReadSample(FIRST_VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample))? };
        let ended = flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0;
        let Some(sample) = sample.filter(|_| !ended) else {
            if let Some(writer) = part.take() {
                writer.finish(audio)?;
            }
            break;
        };
        // Fewer frames: keep one per frame of the new rate.
        if time + frame_duration / 2 < next_frame {
            continue;
        }
        next_frame = next_frame.max(time) + frame_duration;
        let index = plan.parts.iter().position(|&(_, end)| time < end).unwrap_or(plan.parts.len() - 1);
        if part.as_ref().is_none_or(|writer| writer.index != index) {
            if let Some(writer) = part.take() {
                writer.finish(audio)?;
            }
            let copy = folder.join(part_name(stem, index, plan.parts.len()));
            part = Some(PartWriter::open(&copy, index, plan.parts[index], plan)?);
            copies.push(copy);
        }
        if let Some(writer) = part.as_mut() {
            writer.write_video(&sample, time, frame_duration, audio)?;
        }
        progress((time as f32 / total as f32).clamp(0.0, 1.0));
    }
    progress(1.0);
    Ok(copies)
}

/// The clip's video, decoded and scaled to the plan's size, as NV12.
fn open_video(path: &Path, plan: &EncodePlan) -> Result<IMFSourceReader, ShareError> {
    let attributes = attributes(1)?;
    unsafe {
        // Scales to the size asked for.
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes)?;
        reader.SetStreamSelection(ALL_STREAMS, false)?;
        reader.SetStreamSelection(FIRST_VIDEO_STREAM, true)?;
        let nv12 = MFCreateMediaType()?;
        nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        nv12.SetUINT64(&MF_MT_FRAME_SIZE, ratio(plan.width, plan.height))?;
        reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &nv12)?;
        Ok(reader)
    }
}

/// One copy being written: H.264 video and AAC audio in an MP4.
struct PartWriter {
    writer: IMFSinkWriter,
    index: usize,
    /// The part's `(start, end)` in the clip, 100 ns units.
    span: (i64, i64),
    video: u32,
    audio: u32,
    /// Audio written so far, in 48 kHz frames from the part's start.
    audio_frames: usize,
}

impl PartWriter {
    fn open(path: &Path, index: usize, span: (i64, i64), plan: &EncodePlan) -> Result<Self, ShareError> {
        let attributes = attributes(2)?;
        unsafe {
            // The graphics card's encoder when it takes these frames.
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attributes)?;

            let h264 = video_type(plan)?;
            h264.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            h264.SetUINT32(&MF_MT_AVG_BITRATE, plan.video_bitrate)?;
            h264.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            let video = writer.AddStream(&h264)?;
            let nv12 = video_type(plan)?;
            nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            writer.SetInputMediaType(video, &nv12, None)?;

            let aac = audio_type()?;
            aac.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE / 8)?;
            let audio = writer.AddStream(&aac)?;
            let pcm = audio_type()?;
            pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS as u32 * 2)?;
            pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * CHANNELS as u32 * 2)?;
            writer.SetInputMediaType(audio, &pcm, None)?;

            writer.BeginWriting()?;
            Ok(Self { writer, index, span, video, audio, audio_frames: 0 })
        }
    }

    /// Writes a frame shown at `time` in the clip, and the audio up to its end.
    fn write_video(&mut self, sample: &IMFSample, time: i64, duration: i64, audio: &[i16]) -> Result<(), ShareError> {
        let at = (time - self.span.0).max(0);
        unsafe {
            sample.SetSampleTime(at)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(self.video, sample)?;
        }
        self.write_audio_until(at + duration, audio)
    }

    /// Writes the part's audio from where it got to up to `until` (100 ns
    /// from the part's start), in 100 ms samples.
    fn write_audio_until(&mut self, until: i64, audio: &[i16]) -> Result<(), ShareError> {
        let start_frame = (self.span.0 * i64::from(SAMPLE_RATE) / HNS_PER_SECOND) as usize;
        let end_frame = (until.min(self.span.1 - self.span.0) * i64::from(SAMPLE_RATE) / HNS_PER_SECOND) as usize;
        const CHUNK: usize = SAMPLE_RATE as usize / 10;
        while self.audio_frames < end_frame {
            let frames = CHUNK.min(end_frame - self.audio_frames);
            let from = (start_frame + self.audio_frames) * CHANNELS;
            let mut pcm = vec![0i16; frames * CHANNELS];
            if let Some(available) = audio.get(from..) {
                let take = available.len().min(pcm.len());
                pcm[..take].copy_from_slice(&available[..take]);
            }
            let bytes: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
            unsafe {
                let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
                buffer.Unlock()?;
                buffer.SetCurrentLength(bytes.len() as u32)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(self.audio_frames as i64 * HNS_PER_SECOND / i64::from(SAMPLE_RATE))?;
                sample.SetSampleDuration(frames as i64 * HNS_PER_SECOND / i64::from(SAMPLE_RATE))?;
                self.writer.WriteSample(self.audio, &sample)?;
            }
            self.audio_frames += frames;
        }
        Ok(())
    }

    /// Writes the rest of the part's audio and closes the file.
    fn finish(mut self, audio: &[i16]) -> Result<(), ShareError> {
        self.write_audio_until(self.span.1 - self.span.0, audio)?;
        unsafe { self.writer.Finalize()? };
        Ok(())
    }
}

fn attributes(count: u32) -> Result<IMFAttributes, ShareError> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, count)? };
    Ok(attributes.expect("MFCreateAttributes succeeded"))
}

fn video_type(plan: &EncodePlan) -> Result<IMFMediaType, ShareError> {
    unsafe {
        let video = MFCreateMediaType()?;
        video.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        video.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        video.SetUINT64(&MF_MT_FRAME_SIZE, ratio(plan.width, plan.height))?;
        video.SetUINT64(&MF_MT_FRAME_RATE, ratio(plan.fps, 1))?;
        video.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, ratio(1, 1))?;
        Ok(video)
    }
}

fn audio_type() -> Result<IMFMediaType, ShareError> {
    unsafe {
        let audio = MFCreateMediaType()?;
        audio.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        audio.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        audio.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        audio.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
        Ok(audio)
    }
}

/// Two 32-bit values packed as Media Foundation stores sizes and rates.
fn ratio(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    /// A minute of Sonora's 1080p60 recording (12 Mbit/s).
    fn minute_at_1080p60() -> ShareSource {
        ShareSource { duration: 60 * HNS_PER_SECOND, width: 1920, height: 1080, fps: 60.0, video_bytes: 90 * MB, audio_tracks: 6 }
    }

    fn encode(plan: SharePlan) -> EncodePlan {
        match plan {
            SharePlan::Encode(plan) => plan,
            SharePlan::AsIs => panic!("expected an encode"),
        }
    }

    #[test]
    fn a_clip_that_fits_is_copied_as_it_is() {
        assert_eq!(plan_share(&minute_at_1080p60(), 1000 * MB), SharePlan::AsIs);
    }

    #[test]
    fn a_minute_fits_in_twenty_megabytes_at_720p() {
        let plan = encode(plan_share(&minute_at_1080p60(), 20 * MB));
        assert_eq!(plan.parts, [(0, 60 * HNS_PER_SECOND)]);
        assert_eq!((plan.width, plan.height, plan.fps), (1280, 720, 30));
        let bytes = (f64::from(plan.video_bitrate + AUDIO_BITRATE) / 8.0 * 60.0) as u64;
        assert!(bytes <= 20 * MB && bytes > 18 * MB, "fills the limit: {bytes}");
    }

    #[test]
    fn a_long_clip_is_split_into_equal_parts_that_each_fit() {
        let source = ShareSource { duration: 180 * HNS_PER_SECOND, video_bytes: 270 * MB, ..minute_at_1080p60() };
        let plan = encode(plan_share(&source, 20 * MB));
        assert_eq!(plan.parts.len(), 2);
        assert_eq!(plan.parts[1], (90 * HNS_PER_SECOND, 180 * HNS_PER_SECOND));
        assert!(plan.video_bitrate >= MIN_VIDEO_BITRATE);
        assert_eq!(plan.height, 540, "fewer pixels for fewer bits");
    }

    #[test]
    fn a_short_clip_keeps_its_size_and_frame_rate() {
        let source = ShareSource { duration: 20 * HNS_PER_SECOND, video_bytes: 30 * MB, ..minute_at_1080p60() };
        let plan = encode(plan_share(&source, 25 * MB));
        assert_eq!((plan.width, plan.height, plan.fps), (1920, 1080, 60));
        assert!(plan.video_bitrate <= MAX_VIDEO_BITRATE);
    }

    #[test]
    fn a_portrait_clip_stays_portrait() {
        let source = ShareSource { width: 1080, height: 1920, ..minute_at_1080p60() };
        let plan = encode(plan_share(&source, 20 * MB));
        assert_eq!((plan.width, plan.height), (720, 1280));
    }
}
