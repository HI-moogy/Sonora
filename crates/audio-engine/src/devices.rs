use windows::core::PCWSTR;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioClient, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Variant::VT_BOOL;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    /// For an output that's a virtual cable (VB-Audio's): the microphone
    /// its other end shows up as in Discord and games.
    pub voice_cable_mic: Option<String>,
}

// DEVPKEY_Device_FriendlyName, defined in devpkey.h; not bound by the
// `windows` crate under Win32_Media_Audio, so declared here directly.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

// A microphone's "Listen to this device" checkbox (Sound Control Panel,
// Recording, Properties, Listen) and its "Playback through this device"
// choice; undocumented, but where Windows keeps them on the device.
const LISTEN_FMTID: windows::core::GUID = windows::core::GUID::from_u128(0x24dbb0fc_9311_4b3d_9cf0_18ff155639d4);
const PKEY_LISTEN_ENABLED: PROPERTYKEY = PROPERTYKEY { fmtid: LISTEN_FMTID, pid: 1 };
const PKEY_LISTEN_PLAYBACK_DEVICE: PROPERTYKEY = PROPERTYKEY { fmtid: LISTEN_FMTID, pid: 0 };

/// Lists every currently active audio render (output/playback) device.
pub fn list_output_devices() -> windows::core::Result<Vec<AudioDevice>> {
    list_devices(eRender)
}

/// Lists every currently active audio capture (microphone/line-in) device.
pub fn list_input_devices() -> windows::core::Result<Vec<AudioDevice>> {
    list_devices(eCapture)
}

fn list_devices(flow: EDataFlow) -> windows::core::Result<Vec<AudioDevice>> {
    unsafe {
        let collection: IMMDeviceCollection = enumerator()?.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;

        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let device = collection.Item(i)?;
            let id = endpoint_id(&device)?;
            let name = friendly_name(&device).unwrap_or_else(|| id.clone());

            let voice_cable_mic = (flow == eRender && is_virtual_cable(&name)).then(|| "CABLE Output".to_string());
            devices.push(AudioDevice { id, name, voice_cable_mic });
        }
        Ok(devices)
    }
}

/// `device`'s id, as the device lists give it.
pub(crate) fn endpoint_id(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        Ok(text)
    }
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store: IPropertyStore = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME as *const _).ok()?;
        Some(propvariant_to_string(&value))
    }
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

/// The device `device_id` names (from the device lists), or Windows' default
/// for `flow` when `None`.
pub(crate) fn endpoint(device_id: Option<&str>, flow: EDataFlow) -> windows::core::Result<IMMDevice> {
    let enumerator = enumerator()?;
    unsafe {
        match device_id {
            Some(id) => {
                let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            None => enumerator.GetDefaultAudioEndpoint(flow, eConsole),
        }
    }
}

/// The id of Windows' default output device (where "System default" plays).
pub fn default_output_device_id() -> windows::core::Result<String> {
    endpoint_id(&endpoint(None, eRender)?)
}

/// The id of Windows' default microphone.
pub fn default_input_device_id() -> windows::core::Result<String> {
    endpoint_id(&endpoint(None, eCapture)?)
}

/// How Windows records microphone `device_id` (`None`: the default one):
/// its channels and sample rate. Sonora asks for mono at 48 kHz, so Windows
/// converts anything else.
pub fn input_format(device_id: Option<&str>) -> windows::core::Result<(u16, u32)> {
    unsafe {
        let client: IAudioClient = endpoint(device_id, eCapture)?.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let channels_and_rate = ((*format).nChannels, (*format).nSamplesPerSec);
        CoTaskMemFree(Some(format as *const _));
        Ok(channels_and_rate)
    }
}

/// A microphone with Windows' "Listen to this device" on: Windows plays it
/// straight to an output, alongside anything Sonora does with it.
pub struct ListenedInput {
    pub name: String,
    /// The output it plays on; `None` is the default output.
    pub plays_on: Option<String>,
}

/// Every active microphone with "Listen to this device" on. Played into the
/// virtual cable, it's a second, unprocessed copy of your voice there.
pub fn listened_inputs() -> windows::core::Result<Vec<ListenedInput>> {
    unsafe {
        let collection = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut listened = Vec::new();
        for i in 0..collection.GetCount()? {
            let device = collection.Item(i)?;
            let Ok(store) = device.OpenPropertyStore(STGM_READ) else { continue };
            let on = store.GetValue(&PKEY_LISTEN_ENABLED as *const _).is_ok_and(|value| {
                value.Anonymous.Anonymous.vt == VT_BOOL && value.Anonymous.Anonymous.Anonymous.boolVal.0 != 0
            });
            if !on {
                continue;
            }
            let plays_on = store
                .GetValue(&PKEY_LISTEN_PLAYBACK_DEVICE as *const _)
                .map(|value| propvariant_to_string(&value))
                .ok()
                .filter(|id| !id.is_empty());
            let name = friendly_name(&device).unwrap_or_else(|| endpoint_id(&device).unwrap_or_default());
            listened.push(ListenedInput { name, plays_on });
        }
        Ok(listened)
    }
}

/// Whether an output device is VB-Audio Virtual Cable's input side, which
/// apps record from as "CABLE Output".
fn is_virtual_cable(name: &str) -> bool {
    name.contains("CABLE Input") || name.contains("VB-Audio Virtual Cable")
}

unsafe fn propvariant_to_string(value: &PROPVARIANT) -> String {
    // VT_LPWSTR friendly-name values are read via the PWSTR union arm;
    // anything else (unexpected for this property) falls back to empty.
    let pwstr = value.Anonymous.Anonymous.Anonymous.pwszVal;
    if pwstr.is_null() {
        String::new()
    } else {
        pwstr.to_string().unwrap_or_default()
    }
}
