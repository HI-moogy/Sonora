use std::mem::ManuallyDrop;

use windows::core::{Interface, Result, GUID};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High, ICodecAPI, IMFActivate,
    IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform,
    MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFNominalRange_16_235, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
    MFVideoTransFunc_709, MFVideoTransferMatrix_BT709, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, METransformHaveOutput, METransformNeedInput, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX,
    MF_TRANSFORM_ASYNC_UNLOCK,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;

use crate::StartError;

/// One encoded frame as the encoder emitted it (Annex B), with its
/// presentation time in 100 ns units.
pub(crate) struct EncodedFrame {
    pub pts: i64,
    pub annex_b: Vec<u8>,
}

/// The GPU's own H.264 encoder (Media Foundation hardware MFT), fed NV12
/// textures directly from the video processor, so frames never leave VRAM
/// until they're compressed.
pub(crate) struct H264Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    _device_manager: IMFDXGIDeviceManager,
    provides_samples: bool,
    output_buffer_size: u32,
    /// Inputs the encoder has asked for and not yet received.
    input_requests: u32,
    frame_duration: i64,
}

pub(crate) struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

impl H264Encoder {
    pub(crate) fn new(device: &ID3D11Device, config: &EncoderConfig) -> std::result::Result<Self, StartError> {
        let transform = find_hardware_encoder()?;

        let attributes = unsafe { transform.GetAttributes()? };
        unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };

        let mut reset_token = 0u32;
        let mut device_manager = None;
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut device_manager)? };
        let device_manager = device_manager.expect("MFCreateDXGIDeviceManager succeeded without a manager");
        unsafe {
            device_manager.ResetDevice(device, reset_token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, device_manager.as_raw() as usize)?;
        }

        // Rate control has to be configured before the media types are set,
        // or encoders keep their own default bitrate.
        if let Ok(codec) = transform.cast::<ICodecAPI>() {
            // One keyframe per second bounds how far a saved clip's start can
            // drift from the requested length; no B-frames keeps decode order
            // equal to display order, which the MP4 writer assumes.
            set_codec_value(&codec, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32);
            set_codec_value(&codec, &CODECAPI_AVEncCommonMeanBitRate, config.bitrate);
            set_codec_value(&codec, &CODECAPI_AVEncMPVGOPSize, config.fps);
            set_codec_value(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        }
        // Encoders require the output type before the input type.
        let output_type = video_type(&MFVideoFormat_H264, config)?;
        unsafe {
            output_type.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)?;
            output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            // Matches the video processor's BT.709 limited-range output, so
            // players decode colors the way they were encoded.
            output_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            output_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
            output_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            output_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            transform.SetOutputType(0, &output_type, 0)?;
            transform.SetInputType(0, &video_type(&MFVideoFormat_NV12, config)?, 0)?;
        }


        let stream_info = unsafe { transform.GetOutputStreamInfo(0)? };
        let events: IMFMediaEventGenerator = transform.cast()?;
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }

        Ok(Self {
            transform,
            events,
            _device_manager: device_manager,
            provides_samples: stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
            output_buffer_size: stream_info.cbSize.max(1 << 20),
            input_requests: 0,
            frame_duration: 10_000_000 / config.fps as i64,
        })
    }

    /// Handles everything the encoder has signalled since the last call:
    /// records input requests and hands each finished frame to `on_frame`.
    pub(crate) fn pump(&mut self, on_frame: &mut impl FnMut(EncodedFrame)) -> Result<()> {
        loop {
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(error) => return Err(error),
            };
            let kind = unsafe { event.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.input_requests += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                self.collect_output(on_frame)?;
            }
        }
    }

    pub(crate) fn wants_input(&self) -> bool {
        self.input_requests > 0
    }

    /// Submits `texture` if the encoder currently wants input. Returns false
    /// (dropping the frame) if it's still busy with earlier ones.
    pub(crate) fn try_encode(&mut self, texture: &ID3D11Texture2D, pts: i64) -> Result<bool> {
        if self.input_requests == 0 {
            return Ok(false);
        }
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)? };
        let sample = unsafe { MFCreateSample()? };
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.input_requests -= 1;
        Ok(true)
    }

    fn collect_output(&mut self, on_frame: &mut impl FnMut(EncodedFrame)) -> Result<()> {
        let provided = if self.provides_samples {
            None
        } else {
            let sample = unsafe { MFCreateSample()? };
            let buffer = unsafe { MFCreateMemoryBuffer(self.output_buffer_size)? };
            unsafe { sample.AddBuffer(&buffer)? };
            Some(sample)
        };
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(provided),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut status) };
        let sample = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
        unsafe { ManuallyDrop::drop(&mut output[0].pEvents) };

        match result {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let negotiated = unsafe { self.transform.GetOutputAvailableType(0, 0)? };
                unsafe { self.transform.SetOutputType(0, &negotiated, 0)? };
                return Ok(());
            }
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
            Err(error) => return Err(error),
        }

        if let Some(sample) = sample {
            on_frame(read_sample(&sample)?);
        }
        Ok(())
    }
}

fn find_hardware_encoder() -> std::result::Result<IMFTransform, StartError> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )?;
    }
    if activates.is_null() {
        return Err(StartError::NoHardwareEncoder);
    }
    // MFTEnumEx hands back a CoTaskMemAlloc'd array of owned references.
    let found = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
    let transform = found
        .iter()
        .flatten()
        .find_map(|activate| unsafe { activate.ActivateObject::<IMFTransform>().ok() });
    for activate in found.iter_mut() {
        drop(activate.take());
    }
    unsafe { CoTaskMemFree(Some(activates as *const _)) };
    transform.ok_or(StartError::NoHardwareEncoder)
}

fn video_type(subtype: &GUID, config: &EncoderConfig) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType()? };
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    }
    Ok(media_type)
}

/// MFSetAttributeSize / MFSetAttributeRatio packing (high word first).
fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Encoders differ in which codec properties they support; an unsupported
/// one keeps the encoder's default rather than failing setup.
fn set_codec_value(codec: &ICodecAPI, property: &GUID, value: u32) {
    let value = VARIANT::from(value);
    unsafe {
        let _ = codec.SetValue(property, &value);
    }
}

fn read_sample(sample: &IMFSample) -> Result<EncodedFrame> {
    let pts = unsafe { sample.GetSampleTime()? };
    let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
    let mut data = std::ptr::null_mut();
    let mut len = 0u32;
    unsafe { buffer.Lock(&mut data, None, Some(&mut len))? };
    let annex_b = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    unsafe { buffer.Unlock()? };
    Ok(EncodedFrame { pts, annex_b })
}
