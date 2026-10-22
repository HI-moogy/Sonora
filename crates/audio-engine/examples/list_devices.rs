use audio_engine::{list_output_devices, ComGuard};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    for device in list_output_devices()? {
        println!("id={} name={}", device.id, device.name);
    }
    Ok(())
}
