//! Discord streams: where apps the mixer plays put their own sound. Turned
//! down (Sonora plays its processed copy), a game shared as a window on
//! Discord streams silence; parked on an output nobody listens to, it plays
//! there at full volume and the stream has its sound (see
//! [`Mixer::set_silent_output`]).

use std::collections::HashSet;

use audio_engine::{AudioDevice, BusInfo, Mixer};
use eframe::egui;

use crate::theme;

const TURNED_DOWN: &str = "Turned down (streams are silent)";

#[derive(Default)]
pub struct StreamsWindow {
    open: bool,
}

impl StreamsWindow {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, mixer: &Mixer, outputs: &[AudioDevice], buses: &[BusInfo]) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Discord streams").open(&mut open).collapsible(false).resizable(false).default_width(460.0).show(
            ctx,
            |ui| {
                ui.label(
                    "Sharing a game's window on Discord streams that game's own sound. Sonora turns the game's own \
                     sound down while it plays its copy through the EQ, so the stream is silent.",
                );
                ui.add_space(4.0);
                if !audio_engine::per_app_capture_supported() {
                    ui.label(
                        egui::RichText::new("On your Windows apps keep their full volume, so streams have their sound already.")
                            .color(theme::accent()),
                    );
                    return;
                }
                ui.label(
                    "Pick an output you never listen to (a monitor's speakers you don't use, say): apps play their own \
                     sound there at full volume instead, streams hear them, and you still hear them through Sonora.",
                );
                ui.add_space(6.0);

                // Never one you hear: the default output, a channel's, or a
                // virtual cable (that'd go into your microphone).
                let default = audio_engine::devices::default_output_device_id().ok();
                let heard: HashSet<&str> = buses.iter().filter_map(|bus| bus.device_id.as_deref()).collect();
                let choices: Vec<&AudioDevice> = outputs
                    .iter()
                    .filter(|device| Some(&device.id) != default.as_ref() && !heard.contains(device.id.as_str()) && device.voice_cable_mic.is_none())
                    .collect();
                let current = mixer.silent_output();
                let name_of = |id: &str| outputs.iter().find(|device| device.id == id).map_or("an output that's gone", |device| device.name.as_str());
                let mut picked = current.clone();
                ui.horizontal(|ui| {
                    ui.label("Apps' own sound");
                    egui::ComboBox::from_id_salt("silent-output")
                        .width(280.0)
                        .selected_text(current.as_deref().map_or(TURNED_DOWN, name_of))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut picked, None, TURNED_DOWN);
                            for device in &choices {
                                ui.selectable_value(&mut picked, Some(device.id.clone()), &device.name);
                            }
                        });
                });
                if picked != current {
                    mixer.set_silent_output(picked.clone());
                }
                if let Some(silent) = &picked {
                    if Some(silent) == default.as_ref() || heard.contains(silent.as_str()) {
                        ui.colored_label(theme::warning(), "You hear this output now, so apps stay turned down. Pick another.");
                    }
                    ui.label(
                        egui::RichText::new(
                            "Pick one with nothing plugged in: anything playing there is heard from it. A game that's \
                             already playing moves there once it opens its sound again (restart it).",
                        )
                        .small()
                        .color(theme::text_muted()),
                    );
                } else if choices.is_empty() {
                    ui.label(egui::RichText::new("Every output is one you hear.").small().color(theme::text_muted()));
                }
            },
        );
        self.open = open;
    }
}
