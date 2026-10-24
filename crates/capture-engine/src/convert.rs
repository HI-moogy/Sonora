use std::mem::ManuallyDrop;

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
    D3D11_VIDEO_PROCESSOR_CAPS, D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION, D3D11_VIDEO_PROCESSOR_ROTATION,
    D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270, D3D11_VIDEO_PROCESSOR_ROTATION_90,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT_NV12, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
    DXGI_MODE_ROTATION_ROTATE90, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

/// Converts the desktop image (BGRA, desktop resolution) into NV12 at the
/// encode resolution on the GPU's video processor, cycling through a small
/// pool of output textures so the encoder can still be reading earlier
/// frames while the next one is written.
pub(crate) struct Converter {
    video_context: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    input_view: ID3D11VideoProcessorInputView,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
}

/// Enough NV12 textures to cover what a hardware encoder keeps in flight.
const POOL_SIZE: usize = 8;

impl Converter {
    /// `rotation` is how the input was turned from upright (a monitor on its
    /// side); the output is turned back if the video processor can.
    pub(crate) fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: &ID3D11Texture2D,
        (in_width, in_height): (u32, u32),
        rotation: DXGI_MODE_ROTATION,
        (out_width, out_height): (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext1 = context.cast()?;

        let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_width,
            InputHeight: in_height,
            OutputFrameRate: rate,
            OutputWidth: out_width,
            OutputHeight: out_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator: ID3D11VideoProcessorEnumerator =
            unsafe { video_device.CreateVideoProcessorEnumerator(&content)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        unsafe { enumerator.GetVideoProcessorCaps(&mut caps)? };
        let can_rotate = caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 != 0;
        let turn = upright_turn(rotation).filter(|_| can_rotate);
        // The image as it'll look: turned upright, or as it came.
        let shown = match turn {
            Some(D3D11_VIDEO_PROCESSOR_ROTATION_90 | D3D11_VIDEO_PROCESSOR_ROTATION_270) => (in_height, in_width),
            _ => (in_width, in_height),
        };

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut input_view = None;
        unsafe { video_device.CreateVideoProcessorInputView(input, &enumerator, &input_desc, Some(&mut input_view))? };

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut outputs = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let texture = create_nv12(device, out_width, out_height)?;
            let mut view = None;
            unsafe { video_device.CreateVideoProcessorOutputView(&texture, &enumerator, &output_desc, Some(&mut view))? };
            outputs.push((texture, view.expect("CreateVideoProcessorOutputView succeeded without a view")));
        }

        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            video_context.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&RECT { left: 0, top: 0, right: in_width as i32, bottom: in_height as i32 }),
            );
            video_context.VideoProcessorSetStreamDestRect(
                &processor,
                0,
                true,
                Some(&fit_rect(shown, (out_width, out_height))),
            );
            if let Some(turn) = turn {
                video_context.VideoProcessorSetStreamRotation(&processor, 0, true, turn);
            }
            // Black bars when the image's aspect ratio differs from 16:9
            // (a window, or a non-16:9 monitor). Given as RGB: the AMD
            // driver read a YCbCr black as RGB and drew teal bars.
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
            };
            video_context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
        }

        Ok(Self {
            video_context,
            processor,
            input_view: input_view.expect("CreateVideoProcessorInputView succeeded without a view"),
            outputs,
            next: 0,
        })
    }

    /// Converts the current contents of the input texture into the next
    /// pooled NV12 texture and returns it.
    pub(crate) fn convert(&mut self) -> Result<ID3D11Texture2D> {
        let (texture, view) = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ..Default::default()
        };
        let result = unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processor, view, 0, std::slice::from_ref(&stream))
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result?;
        Ok(texture.clone())
    }
}

/// The turn that stands up an image a monitor's `rotation` turned: the
/// same angle (seen on a monitor rotated 270 degrees: a 90 degree turn
/// came out upside down).
fn upright_turn(rotation: DXGI_MODE_ROTATION) -> Option<D3D11_VIDEO_PROCESSOR_ROTATION> {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_90),
        DXGI_MODE_ROTATION_ROTATE180 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_180),
        DXGI_MODE_ROTATION_ROTATE270 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_270),
        _ => None,
    }
}

/// The largest rectangle with the input's aspect ratio that fits the output,
/// centered, with even coordinates (NV12 chroma is 2x2 subsampled).
fn fit_rect((in_w, in_h): (u32, u32), (out_w, out_h): (u32, u32)) -> RECT {
    let scale = (out_w as f64 / in_w as f64).min(out_h as f64 / in_h as f64);
    let w = ((in_w as f64 * scale) as i32) & !1;
    let h = ((in_h as f64 * scale) as i32) & !1;
    let left = ((out_w as i32 - w) / 2) & !1;
    let top = ((out_h as i32 - h) / 2) & !1;
    RECT { left, top, right: left + w, bottom: top + h }
}

fn create_nv12(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
    Ok(texture.expect("CreateTexture2D succeeded without a texture"))
}
