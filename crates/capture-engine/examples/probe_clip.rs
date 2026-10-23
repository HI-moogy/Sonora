//! Decodes a clip with Windows' own MP4 demuxer and H.264/AAC decoders
//! (independent of Sonora's writer) and reports what it finds: video frame
//! count and rate, and for each audio track its length, loudness (RMS) and
//! dominant pitch (from zero crossings). Writes the frame nearest the middle
//! to `<clip>.mid.bmp`.
//! Usage: probe_clip <path.mp4>

use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFSourceReader, MFAudioFormat_Float, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFStartup, MFVideoFormat_RGB32, MFSTARTUP_FULL, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
const MEDIA_SOURCE: u32 = 0xFFFF_FFFF;
const ALL_STREAMS: u32 = 0xFFFF_FFFE;

fn main() -> windows::core::Result<()> {
    let path = std::env::args().nth(1).expect("usage: probe_clip <path.mp4>");
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }

    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 1)? };
    let attributes = attributes.unwrap();
    unsafe { attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)? };
    let reader = unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path.as_str()), &attributes)? };

    let duration = unsafe { reader.GetPresentationAttribute(MEDIA_SOURCE, &MF_PD_DURATION)? };
    let duration_hns = unsafe { duration.Anonymous.Anonymous.Anonymous.uhVal } as i64;
    println!("duration: {:.3}s", duration_hns as f64 / 1e7);

    let mut stream = 0;
    let mut audio_streams = Vec::new();
    while let Ok(native) = unsafe { reader.GetNativeMediaType(stream, 0) } {
        let major = unsafe { native.GetGUID(&MF_MT_MAJOR_TYPE)? };
        let kind = if major == MFMediaType_Video { "video" } else if major == MFMediaType_Audio { "audio" } else { "other" };
        let subtype = unsafe { native.GetGUID(&MF_MT_SUBTYPE)? };
        let selected = unsafe { reader.GetStreamSelection(stream) }.map(|b| b.as_bool()).unwrap_or(false);
        println!("stream {stream}: {kind} subtype {subtype:?}{}", if selected { "  [selected by default]" } else { "" });
        if major == MFMediaType_Audio {
            audio_streams.push(stream);
        }
        stream += 1;
    }
    for &audio in &audio_streams {
        let reader = unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path.as_str()), None)? };
        analyze_audio(&reader, audio)?;
    }

    let rgb = unsafe { MFCreateMediaType()? };
    unsafe {
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &rgb)?;
    }
    let current = unsafe { reader.GetCurrentMediaType(FIRST_VIDEO_STREAM)? };
    let size = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let (width, height) = ((size >> 32) as u32, size as u32);
    let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.map(|s| s as i32).unwrap_or(width as i32 * 4);
    println!("decoded frame size: {width}x{height}, stride {stride}");

    let (mut frames, mut first_ts, mut last_ts, mut saved) = (0u32, None, 0i64, false);
    loop {
        let (mut flags, mut timestamp, mut sample): (u32, i64, Option<IMFSample>) = (0, 0, None);
        unsafe { reader.ReadSample(FIRST_VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))? };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        frames += 1;
        first_ts.get_or_insert(timestamp);
        last_ts = timestamp;
        if !saved && timestamp >= duration_hns / 2 {
            write_bmp(&format!("{path}.mid.bmp"), &sample, width, height, stride)?;
            saved = true;
        }
    }
    let span = (last_ts - first_ts.unwrap_or(0)) as f64 / 1e7;
    println!("decoded {frames} frames over {span:.3}s ({:.2} fps)", (frames.saturating_sub(1)) as f64 / span.max(1e-9));
    if saved {
        println!("middle frame written to {path}.mid.bmp");
    }
    Ok(())
}

fn analyze_audio(reader: &IMFSourceReader, stream: u32) -> windows::core::Result<()> {
    let float = unsafe { MFCreateMediaType()? };
    unsafe {
        reader.SetStreamSelection(ALL_STREAMS, false)?;
        reader.SetStreamSelection(stream, true)?;
        float.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        float.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        reader.SetCurrentMediaType(stream, None, &float)?;
    }
    let mut left: Vec<f32> = Vec::new();
    let mut first_ts = None;
    loop {
        let (mut flags, mut timestamp, mut sample): (u32, i64, Option<IMFSample>) = (0, 0, None);
        unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))? };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        first_ts.get_or_insert(timestamp);
        let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
        let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
        unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
        let floats = unsafe { std::slice::from_raw_parts(data as *const f32, len as usize / 4) };
        left.extend(floats.iter().step_by(2));
        unsafe { buffer.Unlock()? };
    }
    let secs = left.len() as f64 / 48_000.0;
    // Judge level and pitch over the non-silent 10 ms blocks only, so a tone
    // that covers part of a clip still reads correctly.
    let active: Vec<f32> = left
        .chunks(480)
        .filter(|block| block.iter().any(|s| s.abs() > 1e-3))
        .flatten()
        .copied()
        .collect();
    let rms = (active.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / active.len().max(1) as f64).sqrt();
    let crossings = active.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    let pitch = crossings as f64 / 2.0 / (active.len() as f64 / 48_000.0).max(1e-9);
    println!(
        "audio stream {stream}: {secs:.3}s, first sample at {:.3}s, non-silent {:.1}s, RMS {rms:.3}, ~{pitch:.0} Hz",
        first_ts.unwrap_or(0) as f64 / 1e7,
        active.len() as f64 / 48_000.0,
    );
    Ok(())
}

fn write_bmp(path: &str, sample: &IMFSample, width: u32, height: u32, stride: i32) -> windows::core::Result<()> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
    unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };

    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        // Positive stride: top-down rows; negative: bottom-up.
        let src_row = if stride >= 0 { y } else { height as usize - 1 - y };
        let start = src_row * stride.unsigned_abs() as usize;
        pixels.extend_from_slice(&bytes[start..start + row]);
    }
    unsafe { buffer.Unlock()? };

    let mut file = Vec::with_capacity(54 + pixels.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&(width as i32).to_le_bytes());
    file.extend_from_slice(&(-(height as i32)).to_le_bytes()); // top-down
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    file.extend_from_slice(&pixels);
    std::fs::write(path, file).map_err(|e| windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), e.to_string()))
}
