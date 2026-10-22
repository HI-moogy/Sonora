use std::time::Duration;

use audio_engine::{list_output_devices, ComGuard, Mixer};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;

    let devices = list_output_devices()?;
    let headset = devices
        .iter()
        .find(|d| d.name.contains("G733"))
        .expect("expected a G733 device to be present for this test");
    println!("routing target: {}", headset.name);

    let mixer = Mixer::start()?;
    let mic_id = mixer.add_mic_source("Microphone", "Chat")?;
    println!("mic added to Chat bus (default device)");

    std::thread::sleep(Duration::from_millis(500));
    println!(
        "default-routed level: {}",
        mixer.sources().into_iter().find(|s| s.id == mic_id).unwrap().level
    );

    // Route Chat explicitly to the headset while live.
    mixer.set_bus_device("Chat", Some(headset.id.clone()));
    println!("re-routed Chat -> {}", headset.name);
    std::thread::sleep(Duration::from_millis(800));
    println!(
        "headset-routed level: {}",
        mixer.sources().into_iter().find(|s| s.id == mic_id).unwrap().level
    );

    // And back to the system default.
    mixer.set_bus_device("Chat", None);
    std::thread::sleep(Duration::from_millis(500));
    println!("re-routed back to default, still alive");

    mixer.remove_source(mic_id);
    println!("device_routing_demo: completed without error");
    Ok(())
}
