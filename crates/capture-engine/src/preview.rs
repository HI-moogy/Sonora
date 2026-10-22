//! Decodes a saved clip's video for in-app playback, scaled down to preview
//! size, on a background thread that stays a few frames ahead.

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video,
    MFVideoFormat_RGB32, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Variant::VT_I8;

use crate::com::MfThread;
use crate::d3d::create_default_device;

/// Decoded frames kept ready beyond the one on screen.
const FRAMES_AHEAD: usize = 3;
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

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
                if let Err(error) = decode(&path, max_width, &command_rx, &frame_tx, &on_frame) {
                    let _ = frame_tx.send(Err(error.message()));
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

fn decode(
    path: &PathBuf,
    max_width: u32,
    commands: &Receiver<Command>,
    frames: &SyncSender<Result<PreviewFrame, String>>,
    on_frame: &impl Fn(),
) -> windows::core::Result<()> {
    let _mf = MfThread::init()?;
    let reader = open_reader(path, max_width)?;
    let current = unsafe { reader.GetCurrentMediaType(VIDEO_STREAM)? };
    let size = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let (width, height) = ((size >> 32) as usize, size as u32 as usize);
    let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.map(|s| s as i32).unwrap_or(width as i32 * 4);

    // Frames ending at or before this are decoded but not shown (seeking
    // lands on the keyframe before the target).
    let mut skip_before = 0i64;
    let mut seek = 0u64;
    let mut ended = false;
    loop {
        let command = if ended {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return Ok(()),
            }
        } else {
            commands.try_recv().ok()
        };
        match command {
            Some(Command::Stop) => return Ok(()),
            Some(Command::Seek { to, seek: number }) => {
                set_position(&reader, to)?;
                (skip_before, seek, ended) = (to, number, false);
                continue;
            }
            None => {}
        }

        let (mut flags, mut timestamp, mut sample): (u32, i64, Option<IMFSample>) = (0, 0, None);
        unsafe { reader.ReadSample(VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))? };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            ended = true;
            continue;
        }
        let Some(sample) = sample else { continue };
        let duration = unsafe { sample.GetSampleDuration() }.unwrap_or(0);
        if timestamp + duration <= skip_before {
            continue;
        }
        let frame = PreviewFrame { pts: timestamp, seek, width, height, rgba: to_rgba(&sample, width, height, stride)? };

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
                Ok(Command::Seek { to, seek: number }) => {
                    set_position(&reader, to)?;
                    (skip_before, seek, ended) = (to, number, false);
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// A reader for the clip's video only, decoding to RGB32 and scaling to at
/// most `max_width` (falling back to full size if scaling isn't available).
/// Decodes on the GPU where it can: Windows' software decoder keeps
/// ~200 MB of frame buffers in system memory.
fn open_reader(path: &Path, max_width: u32) -> windows::core::Result<IMFSourceReader> {
    open_reader_on(path, max_width, true).or_else(|_| open_reader_on(path, max_width, false))
}

fn open_reader_on(path: &Path, max_width: u32, gpu: bool) -> windows::core::Result<IMFSourceReader> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 2)? };
    let attributes = attributes.expect("MFCreateAttributes succeeded");
    unsafe { attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)? };
    if gpu {
        let device = create_default_device()?;
        let (mut reset_token, mut manager) = (0, None);
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)? };
        let manager = manager.expect("MFCreateDXGIDeviceManager succeeded without a manager");
        unsafe {
            manager.ResetDevice(&device, reset_token)?;
            attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &manager)?;
        }
    }
    let reader = unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes)? };
    unsafe {
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(VIDEO_STREAM, true)?;
    }

    let native = unsafe { reader.GetNativeMediaType(VIDEO_STREAM, 0)? };
    let size = unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let (width, height) = ((size >> 32) as u32, size as u32);
    let rgb = unsafe { MFCreateMediaType()? };
    unsafe {
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    }
    if width > max_width && width > 0 {
        let scaled_height = (height as u64 * max_width as u64 / width as u64) as u32 & !1;
        let scaled = unsafe { MFCreateMediaType()? };
        unsafe {
            rgb.CopyAllItems(&scaled)?;
            scaled.SetUINT64(&MF_MT_FRAME_SIZE, ((max_width & !1) as u64) << 32 | scaled_height as u64)?;
        }
        if unsafe { reader.SetCurrentMediaType(VIDEO_STREAM, None, &scaled) }.is_ok() {
            return Ok(reader);
        }
    }
    unsafe { reader.SetCurrentMediaType(VIDEO_STREAM, None, &rgb)? };
    Ok(reader)
}

fn set_position(reader: &IMFSourceReader, pts: i64) -> windows::core::Result<()> {
    let position = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                Anonymous: PROPVARIANT_0_0_0 { hVal: pts.max(0) },
                ..Default::default()
            }),
        },
    };
    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &position) }
}

/// RGB32 (BGRX rows, top-down for a positive stride) to tightly packed RGBA.
fn to_rgba(sample: &IMFSample, width: usize, height: usize, stride: i32) -> windows::core::Result<Vec<u8>> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
    unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row = if stride >= 0 { y } else { height - 1 - y };
        let start = row * stride.unsigned_abs() as usize;
        let Some(src) = bytes.get(start..start + width * 4) else { break };
        rgba.extend(src.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, _]| [r, g, b, 255]));
    }
    rgba.resize(width * height * 4, 0);
    unsafe { buffer.Unlock()? };
    Ok(rgba)
}
