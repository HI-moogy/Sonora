use std::mem::ManuallyDrop;

use bytes::Bytes;
use windows::core::{Result, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFTransform, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Audio,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::audio::{CHANNELS, SAMPLE_RATE};

/// CLSID_AACMFTEncoder (wmcodecdsp.h); not bound by the `windows` crate.
const CLSID_AAC_ENCODER: GUID = GUID::from_u128(0x93af0c51_2275_45d2_a35b_f2ba21caed00);

/// AAC-LC bitrate per track. The encoder only accepts 96/128/160/192 kbps.
pub(crate) const AAC_BITRATE: u32 = 160_000;

/// Windows' built-in AAC encoder (synchronous MFT): 16-bit stereo PCM in,
/// raw AAC frames (1024 samples each) out.
pub(crate) struct AacEncoder {
    transform: IMFTransform,
    output_size: u32,
}

impl AacEncoder {
    pub(crate) fn new() -> Result<Self> {
        let transform: IMFTransform = unsafe { CoCreateInstance(&CLSID_AAC_ENCODER, None, CLSCTX_INPROC_SERVER)? };

        let input = unsafe { MFCreateMediaType()? };
        let output = unsafe { MFCreateMediaType()? };
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS as u32 * 2)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * CHANNELS as u32 * 2)?;

            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
            output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BITRATE / 8)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?; // raw frames, no ADTS headers
            output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?; // AAC-LC

            transform.SetInputType(0, &input, 0)?;
            transform.SetOutputType(0, &output, 0)?;
        }
        let output_size = unsafe { transform.GetOutputStreamInfo(0)?.cbSize.max(8192) };
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self { transform, output_size })
    }

    /// Encodes `pcm` (interleaved stereo) stamped at `pts` (100 ns units) and
    /// passes each finished AAC frame and its timestamp to `on_frame`.
    pub(crate) fn encode(&mut self, pcm: &[i16], pts: i64, on_frame: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        let byte_len = std::mem::size_of_val(pcm) as u32;
        let buffer = unsafe { MFCreateMemoryBuffer(byte_len)? };
        let mut data = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, data, byte_len as usize);
            buffer.Unlock()?;
            buffer.SetCurrentLength(byte_len)?;
        }
        let frames = (pcm.len() / CHANNELS) as i64;
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(frames * 10_000_000 / SAMPLE_RATE as i64)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }

        loop {
            let out = unsafe { MFCreateSample()? };
            unsafe { out.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)? };
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(out.clone())),
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
                Ok(()) => {
                    let (time, bytes) = read_frame(&out)?;
                    on_frame(time, bytes);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
}

fn read_frame(sample: &IMFSample) -> Result<(i64, Bytes)> {
    let time = unsafe { sample.GetSampleTime()? };
    let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let mut data = std::ptr::null_mut();
    let mut len = 0u32;
    unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
    let bytes = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data, len as usize) });
    unsafe { buffer.Unlock()? };
    Ok((time, bytes))
}
