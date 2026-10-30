use std::time::Duration;

use audio_engine::{ComGuard, Mixer};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;

    // --- Phase 1: mic capture is alive and feeding the mixer ---
    {
        let mixer = Mixer::start()?;
        let id = mixer.add_mic_source("Microphone", "Mic", None)?;
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
        mixer.set_bus_eq_band("Mic", 2, audio_engine::EqBand { gain_db: 6.0, ..audio_engine::DEFAULT_EQ_BANDS[2] });
        mixer.set_bus_eq_band("Mic", 8, audio_engine::EqBand { gain_db: -4.0, ..audio_engine::DEFAULT_EQ_BANDS[8] });
        let bands = mixer
            .buses()
            .into_iter()
            .find(|b| b.name == "Mic")
            .map(|b| b.eq)
            .expect("Mic bus should exist");
        println!("Mic EQ after set: {bands:?}");
        assert!((bands[2].gain_db - 6.0).abs() < 0.001, "band 2 should be 6.0");
        assert!((bands[8].gain_db - (-4.0)).abs() < 0.001, "band 8 should be -4.0");

        mixer.remove_source(id);
    }

    println!("eq_demo: all checks passed");
    Ok(())
}
