use std::mem::ManuallyDrop;

use bytes::Bytes;
use windows::core::{Result, GUID};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFTransform, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Audio, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AAC_PAYLOAD_TYPE,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_USER_DATA,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::audio::{BLOCK_FRAMES, CHANNELS, SAMPLE_RATE};

/// CLSID_AACMFTEncoder (wmcodecdsp.h); not bound by the `windows` crate.
const CLSID_AAC_ENCODER: GUID = GUID::from_u128(0x93af0c51_2275_45d2_a35b_f2ba21caed00);
/// CLSID_MSAACDecMFT (wmcodecdsp.h); not bound by the `windows` crate.
const CLSID_AAC_DECODER: GUID = GUID::from_u128(0x32d186a7_218f_4c75_8876_dd77273a8999);

/// AAC-LC bitrate per track. The encoder only accepts 96/128/160/192 kbps.
pub(crate) const AAC_BITRATE: u32 = 160_000;
/// AAC-LC profile level 2 (ISO/IEC 14496-3), as the encoder writes it.
const AAC_LC_PROFILE_LEVEL: u32 = 0x29;
/// AudioSpecificConfig for AAC-LC, 48 kHz, stereo: object type 2,
/// frequency index 3, channel configuration 2.
const AAC_LC_48K_STEREO_CONFIG: [u8; 2] = [0x11, 0x90];

/// Windows' built-in AAC encoder (synchronous MFT): 16-bit stereo PCM in,
/// raw AAC frames (1024 samples each) out.
pub(crate) struct AacEncoder(Mft);

impl AacEncoder {
    pub(crate) fn new() -> Result<Self> {
        let (input, output) = (pcm_type()?, unsafe { MFCreateMediaType()? });
        unsafe {
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
            output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BITRATE / 8)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?; // raw frames, no ADTS headers
            output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, AAC_LC_PROFILE_LEVEL)?;
        }
        Ok(Self(Mft::new(&CLSID_AAC_ENCODER, &input, &output)?))
    }

    /// Encodes `pcm` (interleaved stereo) stamped at `pts` (100 ns units) and
    /// passes each finished AAC frame and its timestamp to `on_frame`.
    pub(crate) fn encode(&mut self, pcm: &[i16], pts: i64, on_frame: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        let bytes = unsafe { std::slice::from_raw_parts(pcm.as_ptr() as *const u8, std::mem::size_of_val(pcm)) };
        let frames = (pcm.len() / CHANNELS) as i64;
        self.0.process(bytes, pts, frames * 10_000_000 / SAMPLE_RATE as i64, on_frame)
    }

    /// Flushes the frames the encoder still holds once all input is in.
    /// The encoder can't take more input afterwards.
    pub(crate) fn drain(&mut self, on_frame: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        self.0.drain(on_frame)
    }
}

/// Windows' built-in AAC decoder: raw AAC-LC frames (48 kHz stereo, as the
/// encoder writes them) in, 16-bit stereo PCM out.
pub(crate) struct AacDecoder(Mft);

impl AacDecoder {
    pub(crate) fn new() -> Result<Self> {
        let (input, output) = (unsafe { MFCreateMediaType()? }, pcm_type()?);
        // HEAACWAVEINFO's fields after WAVEFORMATEX (payload type, profile
        // level, then reserved), followed by the AudioSpecificConfig.
        let mut user_data = [0u8; 14];
        user_data[2..4].copy_from_slice(&(AAC_LC_PROFILE_LEVEL as u16).to_le_bytes());
        user_data[12..].copy_from_slice(&AAC_LC_48K_STEREO_CONFIG);
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
            input.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            input.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, AAC_LC_PROFILE_LEVEL)?;
            input.SetBlob(&MF_MT_USER_DATA, &user_data)?;
        }
        Ok(Self(Mft::new(&CLSID_AAC_DECODER, &input, &output)?))
    }

    /// Decodes one AAC frame stamped at `pts` (100 ns units) and passes the
    /// PCM (interleaved stereo, little-endian 16-bit) and its timestamp to
    /// `on_pcm`.
    pub(crate) fn decode(&mut self, frame: &[u8], pts: i64, on_pcm: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        self.0.process(frame, pts, BLOCK_FRAMES * 10_000_000 / SAMPLE_RATE as i64, on_pcm)
    }

    pub(crate) fn drain(&mut self, on_pcm: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        self.0.drain(on_pcm)
    }
}

/// 16-bit stereo PCM at the clip sample rate.
fn pcm_type() -> Result<IMFMediaType> {
    let pcm = unsafe { MFCreateMediaType()? };
    unsafe {
        pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS as u32 * 2)?;
        pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * CHANNELS as u32 * 2)?;
    }
    Ok(pcm)
}

/// A synchronous audio MFT that leaves output buffers to its caller.
struct Mft {
    transform: IMFTransform,
    output_size: u32,
}

impl Mft {
    fn new(clsid: &GUID, input: &IMFMediaType, output: &IMFMediaType) -> Result<Self> {
        let transform: IMFTransform = unsafe { CoCreateInstance(clsid, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            transform.SetInputType(0, input, 0)?;
            transform.SetOutputType(0, output, 0)?;
        }
        let output_size = unsafe { transform.GetOutputStreamInfo(0)?.cbSize.max(8192) };
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self { transform, output_size })
    }

    /// Feeds `data` stamped at `pts` and passes on whatever output is ready.
    fn process(&mut self, data: &[u8], pts: i64, duration: i64, on_output: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        let byte_len = data.len() as u32;
        let buffer = unsafe { MFCreateMemoryBuffer(byte_len)? };
        let mut dst = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut dst, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(byte_len)?;
        }
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.pull(on_output)
    }

    fn drain(&mut self, on_output: &mut impl FnMut(i64, Bytes)) -> Result<()> {
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
        self.pull(on_output)
    }

    /// Passes every output the transform has ready to `on_output`.
    fn pull(&mut self, on_output: &mut impl FnMut(i64, Bytes)) -> Result<()> {
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
                    let (time, bytes) = read_sample(&out)?;
                    on_output(time, bytes);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
}

fn read_sample(sample: &IMFSample) -> Result<(i64, Bytes)> {
    let time = unsafe { sample.GetSampleTime()? };
    let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let mut data = std::ptr::null_mut();
    let mut len = 0u32;
    unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
    let bytes = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data, len as usize) });
    unsafe { buffer.Unlock()? };
    Ok((time, bytes))
}
