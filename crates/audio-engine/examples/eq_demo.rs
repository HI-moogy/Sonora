use std::time::Duration;

use audio_engine::{ComGuard, Mixer};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;

    // --- Phase 1: mic capture is alive and feeding the mixer ---
    {
        let mixer = Mixer::start()?;
        let id = mixer.add_mic_source("Microphone", "Mic")?;
        println!("mic source added: id={id}");

        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(500));
            let level = mixer
                .sources()
                .into_iter()
                .find(|s| s.id == id)
                .map(|s| s.level)
                .unwrap_or(-1.0);
            println!("mic level = {level}");
        }

        // --- Phase 2: EQ changes apply and are readable back ---
        mixer.set_bus_eq_band("Mic", 0, 6.0);
        mixer.set_bus_eq_band("Mic", 4, -4.0);
        let gains = mixer
            .buses()
            .into_iter()
            .find(|b| b.name == "Mic")
            .map(|b| b.eq_gains_db)
            .expect("Mic bus should exist");
        println!("Mic EQ after set: {gains:?}");
        assert!((gains[0] - 6.0).abs() < 0.001, "band 0 should be 6.0");
        assert!((gains[4] - (-4.0)).abs() < 0.001, "band 4 should be -4.0");

        // --- Phase 3: persistence ---
        mixer.save_eq_presets().expect("save presets");
        println!("saved presets");

        mixer.remove_source(id);
    } // mixer (and its threads) drop here

    std::thread::sleep(Duration::from_millis(200));

    // --- Phase 4: a fresh Mixer restores the saved EQ for the same bus name ---
    {
        let mixer = Mixer::start()?;
        let id = mixer.add_mic_source("Microphone", "Mic")?;
        let gains = mixer
            .buses()
            .into_iter()
            .find(|b| b.name == "Mic")
            .map(|b| b.eq_gains_db)
            .expect("Mic bus should exist");
        println!("Mic EQ after restart: {gains:?}");
        assert!((gains[0] - 6.0).abs() < 0.001, "band 0 should restore to 6.0");
        assert!((gains[4] - (-4.0)).abs() < 0.001, "band 4 should restore to -4.0");
        mixer.remove_source(id);
    }

    println!("eq_demo: all checks passed");
    Ok(())
}
