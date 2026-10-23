//! The Mic channel's noise removal controls and before/after test.

use audio_engine::{MicCleanup, MicTest};
use common::NoiseSuppression;
use eframe::egui;

const MODES: [(NoiseSuppression, &str, &str); 4] = [
    (NoiseSuppression::Off, "Off", "Your microphone exactly as it sounds."),
    (
        NoiseSuppression::Light,
        "Light",
        "RNNoise: almost no CPU. Removes steady noise like fans and hum; keyboards and voices get through more.",
    ),
    (
        NoiseSuppression::Standard,
        "Standard",
        "DeepFilterNet, gentler: turns keyboards, fans and chatter down by about 24 dB, keeping a little of the room.",
    ),
    (NoiseSuppression::Strong, "Strong", "DeepFilterNet at full strength (the default): removes keyboards, fans and background chatter as fully as it can."),
];

pub fn noise_controls(ui: &mut egui::Ui, cleanup: &MicCleanup, has_mic: bool) {
    let mode = cleanup.mode();
    ui.horizontal(|ui| {
        ui.strong("Noise removal");
        for (choice, label, help) in MODES {
            if ui.selectable_label(mode == choice, label).on_hover_text(help).clicked() {
                cleanup.set_mode(choice);
            }
        }
    });
    let mut far_voices = cleanup.quiet_far_voices();
    if ui
        .checkbox(&mut far_voices, "Turn down other people's voices")
        .on_hover_text(
            "Turns down voices much quieter than yours, like people across the room. \
             It goes by loudness, so someone right next to you still gets through.",
        )
        .changed()
    {
        cleanup.set_quiet_far_voices(far_voices);
    }

    ui.horizontal(|ui| {
        ui.label("Hear the difference");
        if !has_mic {
            ui.weak("add a microphone first");
            return;
        }
        match cleanup.test() {
            MicTest::Recording(progress) => {
                ui.add(egui::ProgressBar::new(progress).desired_width(160.0).text("Recording… talk or type"))
                    .on_hover_text("Talk, type, or let the noise you want gone happen.");
                ui.ctx().request_repaint();
            }
            test => {
                if ui
                    .button(if test == MicTest::Empty { "Record 6 s" } else { "Record again" })
                    .on_hover_text("Records a few seconds of your microphone. Nothing plays back while recording.")
                    .clicked()
                {
                    cleanup.record_test();
                }
                if test == MicTest::Empty {
                    return;
                }
                let playing = match test {
                    MicTest::Playing { cleaned, progress } => {
                        ui.ctx().request_repaint();
                        Some((cleaned, progress))
                    }
                    _ => None,
                };
                for (cleaned, label) in [(false, "Play before"), (true, "Play after")] {
                    if let Some((_, progress)) = playing.filter(|(which, _)| *which == cleaned) {
                        if ui.add(egui::Button::new(format!("Stop ({:.0}%)", progress * 100.0))).clicked() {
                            cleanup.stop_test_playback();
                        }
                    } else if ui
                        .button(label)
                        .on_hover_text(if cleaned {
                            "The recording cleaned with the settings above, on your default output."
                        } else {
                            "The recording as your microphone heard it, on your default output."
                        })
                        .clicked()
                    {
                        cleanup.play_test(cleaned);
                    }
                }
            }
        }
    });
}
