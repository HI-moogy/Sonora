//! Prints every audio session of a process on every active output device:
//! the device, the session's state, volume, and current peak level.
//! Usage: session_probe <pid>

use audio_engine::devices::list_output_devices;
use audio_engine::ComGuard;
use windows::core::Interface;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let pid: u32 = std::env::args()
        .nth(1)
        .expect("pid")
        .parse()
        .expect("pid is a number");
    let names = list_output_devices()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for d in 0..devices.GetCount()? {
            let device = devices.Item(d)?;
            let id = device.GetId()?.to_string().unwrap_or_default();
            let name = names
                .iter()
                .find(|n| n.id == id)
                .map_or(id.as_str(), |n| n.name.as_str());
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let list = manager.GetSessionEnumerator()?;
            for s in 0..list.GetCount()? {
                let control: IAudioSessionControl2 = list.GetSession(s)?.cast()?;
                if control.GetProcessId()? != pid {
                    continue;
                }
                let volume: ISimpleAudioVolume = control.cast()?;
                let meter: IAudioMeterInformation = control.cast()?;
                println!(
                    "{name}: state {:?}, volume {}, muted {}, peak {:.4}",
                    control.GetState()?,
                    volume.GetMasterVolume()?,
                    volume.GetMute()?.as_bool(),
                    meter.GetPeakValue()?
                );
            }
        }
    }
    Ok(())
}
