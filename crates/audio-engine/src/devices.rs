use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eRender, EDataFlow, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, STGM_READ};
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
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection: IMMDeviceCollection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;

        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let device = collection.Item(i)?;
            let id = device.GetId()?.to_string().unwrap_or_default();

            let store: IPropertyStore = device.OpenPropertyStore(STGM_READ)?;
            let name = match store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME as *const _) {
                Ok(value) => propvariant_to_string(&value),
                Err(_) => id.clone(),
            };

            let voice_cable_mic = (flow == eRender && is_virtual_cable(&name)).then(|| "CABLE Output".to_string());
            devices.push(AudioDevice { id, name, voice_cable_mic });
        }
        Ok(devices)
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
