//! Checks that the mixer keeps playing when an output device fails, without
//! unplugging anything: a bus is pointed at the named device, then this
//! demo takes that device over in exclusive mode (which breaks shared-mode
//! streams on it, as unplugging would) and later lets it go. The bus should
//! fall back to the default device and then return. Its audio is a quiet
//! (-66 dB) tone from a child process.
//! Usage: device_failover_demo <part of the device's name>

use std::time::{Duration, Instant};

use audio_engine::loopback::standard_format;
use audio_engine::render::open_render_client_for_device;
use audio_engine::{list_output_devices, ComGuard, Mixer};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, IAudioClient, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_EXCLUSIVE, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

const BUS: &str = "Aux";

/// The tone's child process, stopped however the demo ends.
struct Tone(std::process::Child);

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Plays the quiet tone until killed (run as the child process).
fn play_tone() -> windows::core::Result<()> {
    let (client, render) = open_render_client_for_device(None, &standard_format())?;
    let size = unsafe { client.GetBufferSize()? };
    unsafe { client.Start()? };
    let mut phase = 0f64;
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let free = size - unsafe { client.GetCurrentPadding()? };
        if free == 0 {
            continue;
        }
        let samples = unsafe {
            std::slice::from_raw_parts_mut(render.GetBuffer(free)? as *mut f32, free as usize * 2)
        };
        for frame in samples.chunks_mut(2) {
            let value = (phase.sin() * 0.0005) as f32;
            frame.fill(value);
            phase += std::f64::consts::TAU * 440.0 / 48_000.0;
        }
        unsafe { render.ReleaseBuffer(free, 0)? };
    }
}

/// Opens `device_id` in exclusive mode with the first 48 kHz stereo PCM
/// format it accepts.
fn take_exclusive(device_id: &str) -> windows::core::Result<IAudioClient> {
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let client: IAudioClient = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator
            .GetDevice(PCWSTR(wide.as_ptr()))?
            .Activate(CLSCTX_ALL, None)?
    };
    let mut last_error = None;
    for bits in [16u16, 24, 32] {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 2 * u32::from(bits / 8),
            nBlockAlign: 2 * bits / 8,
            wBitsPerSample: bits,
            cbSize: 0,
        };
        match unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                0,
                100_000,
                100_000,
                &format,
                None,
            )
        } {
            Ok(()) => return Ok(client),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap())
}

/// The devices this process is playing sound on right now (its session's
/// peak meter is above silence there).
fn playing_on() -> windows::core::Result<Vec<String>> {
    let names = list_output_devices()?;
    let mut playing = Vec::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for d in 0..devices.GetCount()? {
            let device = devices.Item(d)?;
            let id = device.GetId()?.to_string().unwrap_or_default();
            let sessions = device
                .Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)?
                .GetSessionEnumerator()?;
            for s in 0..sessions.GetCount()? {
                let control: IAudioSessionControl2 = sessions.GetSession(s)?.cast()?;
                if control.GetProcessId()? == std::process::id()
                    && control.cast::<IAudioMeterInformation>()?.GetPeakValue()? > 0.0
                {
                    playing.push(
                        names
                            .iter()
                            .find(|n| n.id == id)
                            .map_or(id.clone(), |n| n.name.clone()),
                    );
                }
            }
        }
    }
    Ok(playing)
}

fn report(mixer: &Mixer, phase: &str, seconds: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(500));
        let level = mixer
            .buses()
            .into_iter()
            .find(|b| b.name == BUS)
            .map_or(0.0, |b| b.level);
        let devices = playing_on().unwrap_or_default().join(", ");
        println!(
            "{:>4.1}s {phase:<18} {BUS} level {level:.5}  playing on: {devices}",
            start.elapsed().as_secs_f32()
        );
    }
}

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    if std::env::args().nth(1).as_deref() == Some("--tone") {
        return play_tone();
    }
    let wanted = std::env::args().nth(1).expect("part of a device name");
    let device = list_output_devices()?
        .into_iter()
        .find(|d| d.name.contains(&wanted))
        .expect("no active output device with that name");
    println!("test device: {}", device.name);

    let tone = Tone(
        std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--tone")
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    let mixer = Mixer::start()?;
    mixer.ensure_bus(BUS);
    mixer.set_bus_device(BUS, Some(device.id.clone()));
    mixer.add_source(tone.0.id(), "tone", BUS)?;

    report(&mixer, "on test device", 3);
    match take_exclusive(&device.id) {
        Ok(exclusive) => {
            report(&mixer, "device taken over", 4);
            drop(exclusive);
            report(&mixer, "device released", 4);
        }
        Err(error) => println!("couldn't take the device over in exclusive mode: {error}"),
    }
    // Restore the tone's volume before stopping it.
    drop(mixer);
    drop(tone);
    Ok(())
}
