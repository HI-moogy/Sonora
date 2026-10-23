//! Decodes a saved clip's video for in-app playback, scaled down to preview
//! size, on a background thread that stays a few frames ahead.
//!
//! Samples come straight from the MP4 into Windows' H.264 decoder, driven
//! directly in low-latency mode on the CPU, and are scaled to preview size
//! here. For a 1080p60 clip that peaks at ~40 MB for the whole process
//! (preview_clip example), against ~190 MB through a source reader, which
//! runs the decoder frame-threaded with frame buffers per core. Decoding
//! on the GPU would take ~4x less CPU, but its decoder surfaces cost
//! ~100 MB.

use std::fs::File;
use std::io::BufReader;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use mp4::Mp4Reader;
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFSample, IMFTransform, MFCreate2DMediaBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFVideoFormat_H264, MFVideoFormat_NV12, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::com::MfThread;
use crate::edit::{open_mp4, read_layout, ticks_to_hns, EditError};
use crate::h264::append_annex_b;

/// Decoded frames kept ready beyond the one on screen.
const FRAMES_AHEAD: usize = 3;
/// CLSID_CMSH264DecoderMFT (wmcodecdsp.h); not bound by the `windows` crate.
const CLSID_H264_DECODER: GUID = GUID::from_u128(0x62ce7e72_4c71_4d20_b15d_452831a87d9d);

/// One decoded frame, RGBA with opaque alpha.
pub struct PreviewFrame {
    /// Presentation time from the clip's start, in 100 ns units.
    pub pts: i64,
    /// Which seek this frame follows (see [`VideoPreview::seek`]).
    pub seek: u64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

enum Command {
    Seek { to: i64, seek: u64 },
    Stop,
}

/// Background video decoding for a clip. Frames arrive in order from the
/// last seek (or the start); dropping it stops the thread.
pub struct VideoPreview {
    commands: Sender<Command>,
    frames: Receiver<Result<PreviewFrame, String>>,
    seeks: u64,
    worker: Option<JoinHandle<()>>,
}

impl VideoPreview {
    /// Starts decoding `path`, scaled to at most `max_width` pixels wide.
    /// `on_frame` runs whenever a frame becomes available (e.g. to repaint).
    pub fn open(path: &Path, max_width: u32, on_frame: impl Fn() + Send + 'static) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (frame_tx, frames) = mpsc::sync_channel(FRAMES_AHEAD);
        let path = path.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("clip-preview".into())
            .spawn(move || {
                // A damaged file can make the MP4 reader panic: report it
                // like any other failure rather than going quiet.
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    decode(&path, max_width, &command_rx, &frame_tx, &on_frame).map_err(|error| error.to_string())
                }))
                .unwrap_or_else(|_| Err("the video looks damaged".to_string()));
                if let Err(error) = decoded {
                    let _ = frame_tx.send(Err(error));
                    on_frame();
                }
            })
            .expect("spawn clip preview thread");
        Self { commands, frames, seeks: 0, worker: Some(worker) }
    }

    /// Restarts decoding at the frame showing at `pts`. Frames from before
    /// the seek may still arrive; they carry an older `seek` number than
    /// the one returned.
    pub fn seek(&mut self, pts: i64) -> u64 {
        self.seeks += 1;
        let _ = self.commands.send(Command::Seek { to: pts, seek: self.seeks });
        self.seeks
    }

    /// The latest seek number.
    pub fn current_seek(&self) -> u64 {
        self.seeks
    }

    /// The next decoded frame, if one is ready; `Err` if decoding failed.
    pub fn next_frame(&self) -> Option<Result<PreviewFrame, String>> {
        self.frames.try_recv().ok()
    }
}

impl Drop for VideoPreview {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        // Unblock a pending send.
        while self.frames.try_recv().is_ok() {}
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Where decoding stands since the last seek.
struct Position {
    seek: u64,
    /// The next sample to feed the decoder.
    next: usize,
    /// Frames before this are decoded but not shown (seeking starts at the
    /// keyframe before the target).
    skip_before: i64,
    draining: bool,
    ended: bool,
}

impl Position {
    fn start(clip: &ClipVideo, decoder: &H264Decoder, to: i64, seek: u64) -> windows::core::Result<Self> {
        decoder.flush()?;
        let target = clip.frame_at(to);
        Ok(Self { seek, next: clip.keyframe_at_or_before(target), skip_before: clip.times[target], draining: false, ended: false })
    }
}

fn decode(
    path: &Path,
    max_width: u32,
    commands: &Receiver<Command>,
    frames: &SyncSender<Result<PreviewFrame, String>>,
    on_frame: &impl Fn(),
) -> Result<(), EditError> {
    let mut clip = ClipVideo::open(path)?;
    let _mf = MfThread::init()?;
    let mut decoder = H264Decoder::new(clip.width, clip.height)?;
    let scaler = Scaler::new(clip.width as usize, clip.height as usize, max_width as usize);
    let mut annex_b = Vec::new();
    let mut at = Position::start(&clip, &decoder, 0, 0)?;
    loop {
        let command = if at.ended {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return Ok(()),
            }
        } else {
            commands.try_recv().ok()
        };
        match command {
            Some(Command::Stop) => return Ok(()),
            Some(Command::Seek { to, seek }) => {
                at = Position::start(&clip, &decoder, to, seek)?;
                continue;
            }
            None => {}
        }

        let Some(pts) = decoder.receive()? else {
            if at.next < clip.len() {
                clip.read(at.next, &mut annex_b)?;
                decoder.send(&annex_b, clip.times[at.next], clip.times[at.next + 1] - clip.times[at.next])?;
                at.next += 1;
            } else if !at.draining {
                decoder.drain()?;
                at.draining = true;
            } else {
                at.ended = true;
            }
            continue;
        };
        if pts < at.skip_before {
            continue;
        }
        let frame = PreviewFrame { pts, seek: at.seek, width: scaler.width, height: scaler.height, rgba: decoder.to_rgba(&scaler)? };

        // Wait for room, but keep listening for seeks: a frame from before
        // a seek is worthless.
        let mut pending = Ok(frame);
        loop {
            match frames.try_send(pending) {
                Ok(()) => {
                    on_frame();
                    break;
                }
                Err(TrySendError::Disconnected(_)) => return Ok(()),
                Err(TrySendError::Full(frame)) => pending = frame,
            }
            match commands.recv_timeout(Duration::from_millis(10)) {
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(Command::Seek { to, seek }) => {
                    at = Position::start(&clip, &decoder, to, seek)?;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// A clip's video track, read sample by sample.
struct ClipVideo {
    mp4: Mp4Reader<BufReader<File>>,
    track: u32,
    width: u32,
    height: u32,
    /// SPS and PPS in Annex B, sent ahead of every keyframe so decoding can
    /// start at any of them.
    parameter_sets: Vec<u8>,
    /// Each sample's start, then the end of the last, in 100 ns units.
    times: Vec<i64>,
    /// Indices of keyframes, ascending.
    keyframes: Vec<usize>,
}

impl ClipVideo {
    fn open(path: &Path) -> Result<Self, EditError> {
        let mp4 = open_mp4(path)?;
        let track_id = read_layout(&mp4)?.video;
        let track = &mp4.tracks()[&track_id];
        let mut parameter_sets = Vec::new();
        for nal in [track.sequence_parameter_set()?, track.picture_parameter_set()?] {
            parameter_sets.extend_from_slice(&[0, 0, 0, 1]);
            parameter_sets.extend_from_slice(nal);
        }
        let timescale = track.timescale() as i64;
        let stbl = &track.trak.mdia.minf.stbl;
        let mut times = vec![0];
        let mut ticks = 0i64;
        for entry in &stbl.stts.entries {
            for _ in 0..entry.sample_count {
                ticks += entry.sample_delta as i64;
                times.push(ticks_to_hns(ticks, timescale));
            }
        }
        let count = times.len() - 1;
        if count == 0 || count != track.sample_count() as usize {
            return Err(EditError::Unsupported("video sample times don't match its samples"));
        }
        let keyframes = match &stbl.stss {
            Some(stss) => stss.entries.iter().map(|&id| id as usize - 1).filter(|&index| index < count).collect(),
            None => (0..count).collect(),
        };
        let (width, height) = (track.width() as u32, track.height() as u32);
        Ok(Self { track: track_id, width, height, parameter_sets, times, keyframes, mp4 })
    }

    fn len(&self) -> usize {
        self.times.len() - 1
    }

    /// The frame showing at `pts` (the first or last outside the clip).
    fn frame_at(&self, pts: i64) -> usize {
        self.times[..self.len()].partition_point(|&start| start <= pts).saturating_sub(1)
    }

    fn keyframe_at_or_before(&self, index: usize) -> usize {
        let after = self.keyframes.partition_point(|&keyframe| keyframe <= index);
        after.checked_sub(1).map_or(0, |at| self.keyframes[at])
    }

    /// Reads sample `index` into `annex_b`, ready to decode.
    fn read(&mut self, index: usize, annex_b: &mut Vec<u8>) -> Result<(), EditError> {
        let sample = self.mp4.read_sample(self.track, index as u32 + 1)?.ok_or(EditError::Unsupported("missing video sample"))?;
        if sample.rendering_offset != 0 {
            return Err(EditError::Unsupported("video uses frame reordering"));
        }
        annex_b.clear();
        if self.keyframes.binary_search(&index).is_ok() {
            annex_b.extend_from_slice(&self.parameter_sets);
        }
        append_annex_b(&sample.bytes, annex_b).ok_or(EditError::Unsupported("malformed video sample"))
    }
}

/// Windows' H.264 decoder (software, synchronous MFT), H.264 in, NV12 out.
struct H264Decoder {
    transform: IMFTransform,
    /// The sample the decoder writes each picture into.
    output: IMFSample,
    /// Decoded picture size; coded height can exceed the clip's.
    width: usize,
    rows: usize,
}

impl H264Decoder {
    fn new(width: u32, height: u32) -> windows::core::Result<Self> {
        let transform: IMFTransform = unsafe { CoCreateInstance(&CLSID_H264_DECODER, None, CLSCTX_INPROC_SERVER)? };
        // Low latency (no frame threading) has to be chosen before the media
        // types are set: otherwise the decoder keeps a set of frame buffers
        // per core, ~110 MB for 1080p.
        unsafe { transform.GetAttributes()?.SetUINT32(&MF_LOW_LATENCY, 1)? };
        let input = unsafe { MFCreateMediaType()? };
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64)?;
            transform.SetInputType(0, &input, 0)?;
        }
        let mut decoder = Self { transform, output: unsafe { MFCreateSample()? }, width: 0, rows: 0 };
        decoder.set_output_type()?;
        unsafe {
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    /// Picks NV12 output, at whatever size the stream turned out to be.
    fn set_output_type(&mut self) -> windows::core::Result<()> {
        let mut index = 0;
        let output = loop {
            let available = unsafe { self.transform.GetOutputAvailableType(0, index)? };
            if unsafe { available.GetGUID(&MF_MT_SUBTYPE)? } == MFVideoFormat_NV12 {
                break available;
            }
            index += 1;
        };
        unsafe { self.transform.SetOutputType(0, &output, 0)? };
        let size = unsafe { output.GetUINT64(&MF_MT_FRAME_SIZE)? };
        (self.width, self.rows) = ((size >> 32) as usize, size as u32 as usize);
        // A 2D buffer: the decoder fails to copy pictures into a plain one.
        let buffer = unsafe { MFCreate2DMediaBuffer(self.width as u32, self.rows as u32, MFVideoFormat_NV12.data1, false)? };
        unsafe {
            self.output.RemoveAllBuffers()?;
            self.output.AddBuffer(&buffer)
        }
    }

    fn send(&self, data: &[u8], pts: i64, duration: i64) -> windows::core::Result<()> {
        let buffer = unsafe { MFCreateMemoryBuffer(data.len() as u32)? };
        let mut dst = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut dst, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
        }
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            self.transform.ProcessInput(0, &sample, 0)
        }
    }

    /// Decodes the next picture into the output sample and returns its
    /// time, or `None` if the decoder needs more input.
    fn receive(&mut self) -> windows::core::Result<Option<i64>> {
        loop {
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(self.output.clone())),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut status) };
            unsafe {
                ManuallyDrop::drop(&mut output[0].pSample);
                ManuallyDrop::drop(&mut output[0].pEvents);
            }
            match result {
                Ok(()) => return Ok(Some(unsafe { self.output.GetSampleTime()? })),
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.set_output_type()?,
                Err(error) => return Err(error),
            }
        }
    }

    fn flush(&self) -> windows::core::Result<()> {
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }
    }

    /// Asks for the pictures the decoder still holds once all input is in.
    fn drain(&self) -> windows::core::Result<()> {
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
    }

    /// The last picture [`receive`](Self::receive) returned, scaled.
    fn to_rgba(&self, scaler: &Scaler) -> Result<Vec<u8>, EditError> {
        if self.width < scaler.source_width || self.rows < scaler.source_height {
            return Err(EditError::Unsupported("video is smaller than its header says"));
        }
        let buffer: IMF2DBuffer = unsafe { self.output.GetBufferByIndex(0)?.cast()? };
        let (mut data, mut pitch) = (std::ptr::null_mut(), 0i32);
        unsafe { buffer.Lock2D(&mut data, &mut pitch)? };
        // Top-down NV12 (the buffer was made that way): luma rows, then
        // interleaved chroma rows.
        let pitch = pitch as usize;
        let nv12 = unsafe { std::slice::from_raw_parts(data, pitch * self.rows * 3 / 2) };
        let (luma, chroma) = nv12.split_at(pitch * self.rows);
        let rgba = scaler.nv12_to_rgba(luma, chroma, pitch);
        unsafe { buffer.Unlock2D()? };
        Ok(rgba)
    }
}

/// Downscales NV12 pictures to RGBA by averaging each output pixel's area
/// of the source (2x2 luma and one chroma sample when halving).
struct Scaler {
    width: usize,
    height: usize,
    source_width: usize,
    source_height: usize,
    columns: Vec<Span>,
    lines: Vec<Span>,
}

/// The source luma (and chroma) range one output column or row covers.
struct Span {
    start: usize,
    end: usize,
    chroma_start: usize,
    chroma_end: usize,
    /// 1 / its length, luma then chroma, in 0.16 fixed point.
    scale: u64,
    chroma_scale: u64,
}

impl Span {
    fn all(out: usize, source: usize) -> Vec<Self> {
        let reciprocal = |length: usize| ((1 << 16) + length as u64 / 2) / length as u64;
        (0..out)
            .map(|i| {
                let start = i * source / out;
                let end = ((i + 1) * source / out).max(start + 1);
                let (chroma_start, chroma_end) = (start / 2, end.div_ceil(2));
                Span {
                    start,
                    end,
                    chroma_start,
                    chroma_end,
                    scale: reciprocal(end - start),
                    chroma_scale: reciprocal(chroma_end - chroma_start),
                }
            })
            .collect()
    }
}

impl Scaler {
    fn new(source_width: usize, source_height: usize, max_width: usize) -> Self {
        let (width, height) = if source_width > max_width && max_width > 0 {
            (max_width, (source_height * max_width / source_width).max(1))
        } else {
            (source_width, source_height)
        };
        Self {
            width,
            height,
            source_width,
            source_height,
            columns: Span::all(width, source_width),
            lines: Span::all(height, source_height),
        }
    }

    /// `luma` and `chroma` hold the NV12 planes (chroma interleaved), rows
    /// `pitch` bytes apart, at least the source size. Colors are BT.709
    /// limited range, as Sonora records them.
    fn nv12_to_rgba(&self, luma: &[u8], chroma: &[u8], pitch: usize) -> Vec<u8> {
        // Per output row: sum the source rows it covers column by column,
        // then turn those into running totals along the row, so any output
        // pixel's area sum is one subtraction.
        let chroma_width = self.source_width.div_ceil(2);
        let mut column_sums = vec![0u32; self.source_width.max(chroma_width * 2)];
        let mut luma_totals = vec![0u32; self.source_width + 1];
        let (mut u_totals, mut v_totals) = (vec![0u32; chroma_width + 1], vec![0u32; chroma_width + 1]);
        let (mut ys, mut us, mut vs) = (vec![0i32; self.width], vec![0i32; self.width], vec![0i32; self.width]);
        let mut rgba = vec![0u8; self.width * self.height * 4];
        let sum_rows = |sums: &mut [u32], plane: &[u8], rows: std::ops::Range<usize>| {
            sums.fill(0);
            let width = sums.len();
            for row in rows {
                for (sum, &value) in sums.iter_mut().zip(&plane[row * pitch..row * pitch + width]) {
                    *sum += value as u32;
                }
            }
        };
        fn running_totals(totals: &mut [u32], values: impl Iterator<Item = u32>) {
            let mut total = 0;
            for (slot, value) in totals[1..].iter_mut().zip(values) {
                total += value;
                *slot = total;
            }
        }
        for (line, out) in self.lines.iter().zip(rgba.chunks_exact_mut(self.width * 4)) {
            sum_rows(&mut column_sums[..self.source_width], luma, line.start..line.end);
            running_totals(&mut luma_totals, column_sums.iter().copied());
            sum_rows(&mut column_sums[..chroma_width * 2], chroma, line.chroma_start..line.chroma_end);
            let pairs = column_sums[..chroma_width * 2].as_chunks::<2>().0;
            running_totals(&mut u_totals, pairs.iter().map(|pair| pair[0]));
            running_totals(&mut v_totals, pairs.iter().map(|pair| pair[1]));
            let average = |totals: &[u32], start: usize, end: usize, scale: u64| {
                (((totals[end] - totals[start]) as u64 * scale + (1 << 31)) >> 32) as i32
            };
            for (((y, u), v), column) in ys.iter_mut().zip(&mut us).zip(&mut vs).zip(&self.columns) {
                let chroma_scale = column.chroma_scale * line.chroma_scale;
                *y = average(&luma_totals, column.start, column.end, column.scale * line.scale);
                *u = average(&u_totals, column.chroma_start, column.chroma_end, chroma_scale);
                *v = average(&v_totals, column.chroma_start, column.chroma_end, chroma_scale);
            }
            for (((out, &y), &u), &v) in out.as_chunks_mut::<4>().0.iter_mut().zip(&ys).zip(&us).zip(&vs) {
                *out = bt709_to_rgba(y, u, v);
            }
        }
        rgba
    }
}

/// One BT.709 limited-range pixel to RGBA, in 8.8 fixed point.
fn bt709_to_rgba(y: i32, u: i32, v: i32) -> [u8; 4] {
    let y = (y - 16) * 298;
    let (u, v) = (u - 128, v - 128);
    let channel = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
    [channel(y + 459 * v), channel(y - 55 * u - 136 * v), channel(y + 541 * u), 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halving_averages_two_by_two_luma_with_its_chroma_sample() {
        let scaler = Scaler::new(4, 2, 2);
        assert_eq!((scaler.width, scaler.height), (2, 1));
        // Two luma rows (pitch 4), then one chroma row: black-ish then white.
        let nv12 = [16, 16, 235, 235, 16, 16, 235, 235, 128, 128, 128, 128];
        let (luma, chroma) = nv12.split_at(8);
        assert_eq!(scaler.nv12_to_rgba(luma, chroma, 4), [0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn limited_range_primaries_map_to_full_range() {
        assert_eq!(bt709_to_rgba(235, 128, 128), [255, 255, 255, 255]);
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let [r, g, b, _] = bt709_to_rgba(63, 102, 240);
        assert!(r >= 253 && g <= 2 && b <= 2, "{r} {g} {b}");
    }
}

