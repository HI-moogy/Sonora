use std::mem::{size_of, ManuallyDrop};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{implement, w, Error, Interface, Ref, Result, HRESULT};
use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, WAVEFORMATEX,
};

/// WAVE_FORMAT_IEEE_FLOAT from mmreg.h; not bound as a named constant by
/// the `windows` crate's Media::Audio module.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, BLOB};
use windows::Win32::System::Variant::VT_BLOB;

/// Device interface path that selects Windows' process-loopback virtual
/// audio device, documented in `audioclientactivationparams.h` and not
/// otherwise exposed as a named constant by the `windows` crate.
const VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK: windows::core::PCWSTR = w!("VAD\\Process_Loopback");

/// The first Windows build with per-app (process loopback) capture: 20348,
/// so Windows 11. Windows 10 22H2 is build 19045.
const FIRST_BUILD_WITH_APP_CAPTURE: u32 = 20348;

/// Whether Windows can capture one app's audio, which the mixer needs to
/// process an app (EQ, boosting it). Without it, apps can only play
/// straight on their channel's device. `SONORA_NO_APP_CAPTURE` set makes
/// Sonora act as if it couldn't, to try that on a newer Windows.
pub fn per_app_capture_supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        std::env::var_os("SONORA_NO_APP_CAPTURE").is_none()
            && windows_build().is_none_or(|build| build >= FIRST_BUILD_WITH_APP_CAPTURE)
    })
}

/// Windows' build number, as it reports it in the registry.
fn windows_build() -> Option<u32> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buffer = [0u16; 32];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let len = (size as usize / 2).saturating_sub(1);
    String::from_utf16_lossy(&buffer[..len]).trim().parse().ok()
}

/// The format every capture/render stream in this engine is standardized
/// on, so sources can be mixed without per-source resampling.
pub fn standard_format() -> WAVEFORMATEX {
    let channels = 2u16;
    let sample_rate = 48_000u32;
    let bits_per_sample = 32u16;
    let block_align = channels * (bits_per_sample / 8);
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: channels,
        nSamplesPerSec: sample_rate,
        nAvgBytesPerSec: sample_rate * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits_per_sample,
        cbSize: 0,
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    sender: Mutex<Option<mpsc::Sender<Result<IAudioClient>>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        let result = (|| -> Result<IAudioClient> {
            let operation = activateoperation.ok()?;
            let mut hr = HRESULT(0);
            let mut iface = None;
            unsafe { operation.GetActivateResult(&mut hr, &mut iface) }?;
            hr.ok()?;
            iface.ok_or_else(|| Error::from(E_UNEXPECTED))?.cast()
        })();

        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send(result);
        }
        Ok(())
    }
}

/// Activates a virtual capture client that yields only the audio rendered
/// by `target_process_id` and its child processes, independent of which
/// physical output device Windows is currently using. This is the same
/// mechanism OBS uses for "Capture Audio Output (per application)" and
/// needs no driver or elevated privileges.
pub fn activate_process_loopback_client(target_process_id: u32) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_process_id,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    // PROPVARIANT's Drop impl runs PropVariantClear, which unconditionally
    // CoTaskMemFrees blob.pBlobData for a VT_BLOB variant. It must therefore
    // point at a CoTaskMemAlloc'd heap block, never a stack local, or that
    // free corrupts the heap.
    let size = size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>();
    let blob_data = unsafe { CoTaskMemAlloc(size) } as *mut u8;
    if blob_data.is_null() {
        return Err(Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(&params as *const _ as *const u8, blob_data, size);
    }

    let activation_params = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size as u32,
                        pBlobData: blob_data,
                    },
                },
            }),
        },
    };

    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        sender: Mutex::new(Some(tx)),
    }
    .into();

    // The returned operation handle itself isn't needed past this call:
    // completion is delivered to `handler` and forwarded over `rx`.
    let _operation: IActivateAudioInterfaceAsyncOperation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&activation_params as *const _),
            &handler,
        )?
    };

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| Error::from(E_UNEXPECTED))?
}
