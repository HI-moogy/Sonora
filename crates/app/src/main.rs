use std::collections::HashSet;
use std::time::{Duration, Instant};

use audio_engine::{
    enumerate_sessions, list_output_devices, Mixer, OutputDevice, EQ_BAND_COUNT, EQ_BAND_FREQUENCIES,
    EQ_GAIN_RANGE_DB,
};
use eframe::egui;

mod single_instance;

/// The five channels always shown as faders, matching SteelSeries Sonar's
/// own channel naming plus a dedicated Mic channel.
const FIXED_BUSES: &[&str] = &["Game", "Chat", "Aux", "Media", "Mic"];
// Placeholder palette — not final, just enough to tell faders apart.
const BUS_COLORS: [egui::Color32; 5] = [
    egui::Color32::from_rgb(90, 170, 230),
    egui::Color32::from_rgb(220, 90, 90),
    egui::Color32::from_rgb(90, 110, 200),
    egui::Color32::from_rgb(230, 200, 140),
    egui::Color32::from_rgb(230, 150, 80),
];

const SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(1000);
const DEVICE_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// Redraw rate while any meter is moving vs. while everything is silent.
/// egui still repaints immediately on input, so the idle rate only bounds
/// how quickly meters wake up when audio starts.
const ACTIVE_REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const IDLE_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
const METER_ACTIVITY_THRESHOLD: f32 = 0.001;

struct AvailableSession {
    pid: u32,
    name: String,
}

struct MixerApp {
    mixer: Mixer,
    available_sessions: Vec<AvailableSession>,
    last_session_refresh: Instant,
    output_devices: Vec<OutputDevice>,
    last_device_refresh: Instant,
    selected_bus: Option<String>,
    save_status: Option<String>,
}

impl MixerApp {
    // eframe/winit has already initialized COM (apartment-threaded) on this
    // thread by the time this runs; enumerate_sessions's plain CoCreateInstance
    // calls work fine under that apartment, so this thread doesn't need its
    // own ComGuard. Only the mixer's dedicated capture/render threads need
    // (and independently set up) their own COM apartment.
    fn new() -> windows::core::Result<Self> {
        let mixer = Mixer::start()?;
        for bus in FIXED_BUSES {
            mixer.ensure_bus(bus);
        }
        Ok(Self {
            mixer,
            available_sessions: Vec::new(),
            last_session_refresh: Instant::now() - SESSION_REFRESH_INTERVAL,
            output_devices: Vec::new(),
            last_device_refresh: Instant::now() - DEVICE_REFRESH_INTERVAL,
            selected_bus: None,
            save_status: None,
        })
    }

    fn refresh_sessions(&mut self) {
        if self.last_session_refresh.elapsed() < SESSION_REFRESH_INTERVAL {
            return;
        }
        self.last_session_refresh = Instant::now();

        let Ok(sessions) = enumerate_sessions() else {
            return;
        };
        let own_pid = std::process::id();
        let mut sessions: Vec<AvailableSession> = sessions
            .into_iter()
            .filter(|s| s.process_id != 0 && s.process_id != own_pid && !s.display_name.is_empty())
            .map(|s| AvailableSession {
                pid: s.process_id,
                name: s.display_name,
            })
            .collect();
        sessions.sort_by_key(|s| s.pid);
        sessions.dedup_by_key(|s| s.pid);
        self.available_sessions = sessions;
    }

    fn refresh_devices(&mut self) {
        if self.last_device_refresh.elapsed() < DEVICE_REFRESH_INTERVAL {
            return;
        }
        self.last_device_refresh = Instant::now();
        if let Ok(devices) = list_output_devices() {
            self.output_devices = devices;
        }
    }
}

fn level_meter(ui: &mut egui::Ui, level: f32) {
    ui.add(
        egui::ProgressBar::new(level.clamp(0.0, 1.0))
            .desired_width(90.0)
            .desired_height(10.0),
    );
}

fn eq_controls(ui: &mut egui::Ui, mixer: &Mixer, bus_name: &str, gains_db: [f32; EQ_BAND_COUNT], save_status: &mut Option<String>) {
    ui.horizontal(|ui| {
        for band in 0..EQ_BAND_COUNT {
            ui.vertical(|ui| {
                let freq = EQ_BAND_FREQUENCIES[band];
                let label = if freq >= 1000.0 {
                    format!("{:.1}k", freq / 1000.0)
                } else {
                    format!("{freq:.0}")
                };
                let mut gain = gains_db[band];
                let changed = ui
                    .add(
                        egui::Slider::new(&mut gain, -EQ_GAIN_RANGE_DB..=EQ_GAIN_RANGE_DB)
                            .vertical()
                            .text(label),
                    )
                    .changed();
                if changed {
                    mixer.set_bus_eq_band(bus_name, band, gain);
                }
            });
        }
    });
    if ui.button("Save EQ presets").clicked() {
        *save_status = Some(match mixer.save_eq_presets() {
            Ok(()) => "Saved.".to_string(),
            Err(e) => format!("Save failed: {e}"),
        });
    }
}

impl eframe::App for MixerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_sessions();
        self.refresh_devices();

        let buses = self.mixer.buses();
        let sources = self.mixer.sources();

        let meters_active = self.mixer.master_level() > METER_ACTIVITY_THRESHOLD
            || buses.iter().any(|b| b.level > METER_ACTIVITY_THRESHOLD)
            || sources.iter().any(|s| s.level > METER_ACTIVITY_THRESHOLD);
        ui.ctx().request_repaint_after(if meters_active {
            ACTIVE_REPAINT_INTERVAL
        } else {
            IDLE_REPAINT_INTERVAL
        });
        let mic_already_added = sources.iter().any(|s| s.pid.is_none());
        let active_pids: HashSet<u32> = sources.iter().filter_map(|s| s.pid).collect();

        // --- Detail panel for the selected channel (device, EQ, app routing) ---
        if let Some(selected) = self.selected_bus.clone() {
            egui::Panel::right("bus_detail").min_size(380.0).show(ui, |ui| {
                ui.heading(&selected);
                if let Some(status) = &self.save_status {
                    ui.label(status.clone());
                }
                ui.separator();

                let Some(bus) = buses.iter().find(|b| b.name == selected) else {
                    return;
                };

                ui.horizontal(|ui| {
                    ui.strong("Output device");
                    let current_name = bus
                        .device_id
                        .as_ref()
                        .and_then(|id| self.output_devices.iter().find(|d| &d.id == id))
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| "System Default".to_string());
                    egui::ComboBox::from_id_salt("device-choice")
                        .selected_text(current_name)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(bus.device_id.is_none(), "System Default").clicked() {
                                self.mixer.set_bus_device(&selected, None);
                            }
                            for device in &self.output_devices {
                                let is_selected = bus.device_id.as_deref() == Some(device.id.as_str());
                                if ui.selectable_label(is_selected, &device.name).clicked() {
                                    self.mixer.set_bus_device(&selected, Some(device.id.clone()));
                                }
                            }
                        });
                });
                ui.separator();

                ui.collapsing("EQ", |ui| {
                    eq_controls(ui, &self.mixer, &selected, bus.eq_gains_db, &mut self.save_status);
                });
                ui.separator();

                if selected == "Mic" && !mic_already_added {
                    if ui.button("Add Microphone").clicked() {
                        let _ = self.mixer.add_mic_source("Microphone", "Mic");
                    }
                    ui.separator();
                }

                ui.strong("Apps using this");
                for source in sources.iter().filter(|s| s.bus == selected) {
                    ui.horizontal(|ui| {
                        let pid_label = source.pid.map(|p| p.to_string()).unwrap_or_else(|| "mic".into());
                        ui.label(format!("{} ({pid_label})", source.name));
                        let mut muted = source.muted;
                        if ui.checkbox(&mut muted, "mute").changed() {
                            self.mixer.set_source_muted(source.id, muted);
                        }
                        let mut gain = source.gain;
                        if ui.add(egui::Slider::new(&mut gain, 0.0..=2.0)).changed() {
                            self.mixer.set_source_gain(source.id, gain);
                        }
                        level_meter(ui, source.level);
                        if ui.button("Remove").clicked() {
                            self.mixer.remove_source(source.id);
                        }
                    });
                }

                ui.separator();
                ui.strong("Apps not using this");
                for session in &self.available_sessions {
                    if active_pids.contains(&session.pid) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.label(format!("{} ({})", session.name, session.pid));
                        if ui.button("Add").clicked() {
                            let _ = self.mixer.add_source(session.pid, session.name.clone(), selected.clone());
                        }
                    });
                }
            });
        }

        // --- Main fader bank ---
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("sonar-rs");
            ui.separator();

            ui.horizontal(|ui| {
                ui.strong("Master");
                let mut gain = self.mixer.master_gain();
                if ui.add(egui::Slider::new(&mut gain, 0.0..=2.0).text("gain")).changed() {
                    self.mixer.set_master_gain(gain);
                }
                level_meter(ui, self.mixer.master_level());
            });
            ui.add_space(24.0);

            ui.horizontal(|ui| {
                for (i, &bus_name) in FIXED_BUSES.iter().enumerate() {
                    let bus = buses.iter().find(|b| b.name == bus_name);
                    let mut gain = bus.map(|b| b.gain).unwrap_or(1.0);
                    let level = bus.map(|b| b.level).unwrap_or(0.0);
                    let color = BUS_COLORS[i % BUS_COLORS.len()];

                    ui.vertical(|ui| {
                        ui.set_width(70.0);
                        egui::Frame::new().fill(color.gamma_multiply(0.35)).inner_margin(6.0).show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                let slider = egui::Slider::new(&mut gain, 2.0..=0.0)
                                    .vertical()
                                    .show_value(false);
                                if ui.add_sized([36.0, 180.0], slider).changed() {
                                    self.mixer.set_bus_gain(bus_name, gain);
                                }
                                level_meter(ui, level);
                                let selected = self.selected_bus.as_deref() == Some(bus_name);
                                if ui.selectable_label(selected, bus_name).clicked() {
                                    self.selected_bus = Some(bus_name.to_string());
                                }
                            });
                        });
                    });
                }
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    let Some(_instance) = single_instance::acquire() else {
        return Ok(());
    };
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("bundled icon.png is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        single_instance::WINDOW_TITLE,
        options,
        Box::new(|_cc| {
            let app = MixerApp::new().expect("failed to start audio mixer");
            Ok(Box::new(app))
        }),
    )
}
