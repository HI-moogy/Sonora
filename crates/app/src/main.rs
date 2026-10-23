// Release builds are a GUI app only: no console window alongside.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_engine::{
    enumerate_sessions, list_input_devices, list_output_devices, AudioDevice, Mixer, EQ_BAND_COUNT,
    EQ_BAND_FREQUENCIES, EQ_GAIN_RANGE_DB,
};
use eframe::egui;

mod autostart;
mod clips;
mod eq_presets;
mod games;
mod hotkey;
mod meter;
mod moments;
mod setup;
mod single_instance;
mod sounds;
mod tray;

use meter::level_meter;
use moments::Moments;
use setup::MixerPersistence;
use tray::{Tray, TrayCommand};

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
/// How often to retry restoring apps a crashed run left turned down, for
/// apps that weren't running yet the last time.
const ROUTE_RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

/// Redraw rate while any meter is moving vs. while everything is silent.
/// egui still repaints immediately on input, so the idle rate only bounds
/// how quickly meters wake up when audio starts.
const ACTIVE_REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const IDLE_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
const METER_ACTIVITY_THRESHOLD: f32 = 0.001;
/// While hidden to the tray nothing is drawn; the app only wakes this often
/// for its background work (routing apps as they start, restoring volumes).
const HIDDEN_WAKE_INTERVAL: Duration = Duration::from_secs(1);
/// How many first frames a sign-in start keeps its window hidden for.
const START_HIDDEN_FRAMES: u32 = 5;

struct AvailableSession {
    pid: u32,
    name: String,
}

#[derive(PartialEq, Eq)]
enum Tab {
    Mixer,
    Clips,
}

struct MixerApp {
    mixer: Mixer,
    moments: Moments,
    tab: Tab,
    available_sessions: Vec<AvailableSession>,
    last_session_refresh: Instant,
    output_devices: Vec<AudioDevice>,
    input_devices: Vec<AudioDevice>,
    last_device_refresh: Instant,
    last_route_recovery: Instant,
    selected_bus: Option<String>,
    /// The last action that failed, shown in the channel panel.
    status: Option<String>,
    persistence: MixerPersistence,
    /// Problems loading settings, shown until dismissed.
    config_notices: Vec<String>,
    /// `None` if the tray icon couldn't be added; closing the window then
    /// quits, since there'd be no way to bring it back.
    tray: Option<Tray>,
    /// Set by the tray's Quit, so closing the window really exits.
    quitting: Arc<AtomicBool>,
    /// Whether this run already said it keeps running in the tray.
    tray_hint_shown: bool,
    eq_presets: EqPresetState,
    /// The main window's handle, found on first use.
    window: Option<windows::Win32::Foundation::HWND>,
    /// Started at sign-in: frames left during which to keep the window
    /// hidden, since eframe shows it once after its first frame whatever it
    /// was created with.
    start_hidden_frames: u32,
}

impl MixerApp {
    // eframe/winit has already initialized COM (apartment-threaded) on this
    // thread by the time this runs; enumerate_sessions's plain CoCreateInstance
    // calls work fine under that apartment, so this thread doesn't need its
    // own ComGuard. Only the mixer's dedicated capture/render threads need
    // (and independently set up) their own COM apartment.
    fn new(ctx: &egui::Context, start_hidden: bool) -> windows::core::Result<Self> {
        let mixer = Mixer::start()?;
        for bus in FIXED_BUSES {
            mixer.ensure_bus(bus);
        }
        let persistence = MixerPersistence::restore(&mixer);
        let moments = Moments::new(ctx, &mixer);
        let quitting = Arc::new(AtomicBool::new(false));
        let tray = {
            let ctx = ctx.clone();
            let quitting = quitting.clone();
            Tray::start(move |command| {
                match command {
                    TrayCommand::Open => tray::show_main_window(),
                    TrayCommand::Quit => {
                        if tray::confirm_quit() {
                            quitting.store(true, Ordering::Relaxed);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
                ctx.request_repaint();
            })
            .ok()
        };
        Ok(Self {
            mixer,
            moments,
            tab: Tab::Mixer,
            available_sessions: Vec::new(),
            last_session_refresh: Instant::now() - SESSION_REFRESH_INTERVAL,
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            last_device_refresh: Instant::now() - DEVICE_REFRESH_INTERVAL,
            last_route_recovery: Instant::now(),
            selected_bus: None,
            status: None,
            persistence,
            config_notices: common::take_config_notices(),
            tray,
            quitting,
            tray_hint_shown: false,
            eq_presets: EqPresetState::load(),
            window: None,
            start_hidden_frames: if start_hidden { START_HIDDEN_FRAMES } else { 0 },
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
        if let Ok(devices) = list_input_devices() {
            self.input_devices = devices;
        }
    }
}

/// Your saved EQ presets, and the name being typed to save one.
#[derive(Default)]
struct EqPresetState {
    saved: Vec<common::NamedEq>,
    naming: Option<String>,
    /// Focus the name field on the frame it appears.
    focus_name: bool,
    error: Option<String>,
}

impl EqPresetState {
    fn load() -> Self {
        Self { saved: common::load_presets().eq_presets, ..Self::default() }
    }

    fn store(&mut self) {
        let saved = self.saved.clone();
        self.error = common::update_presets(|presets| presets.eq_presets = saved)
            .err()
            .map(|e| format!("Couldn't save presets: {e}"));
    }
}

/// The preset menu with saving and deleting your own presets.
fn eq_preset_controls(
    ui: &mut egui::Ui,
    mixer: &Mixer,
    bus_name: &str,
    gains_db: [f32; EQ_BAND_COUNT],
    presets: &mut EqPresetState,
) {
    let current = eq_presets::matching(&gains_db, &presets.saved).map(str::to_string);
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label("Preset");
        egui::ComboBox::from_id_salt("eq-preset")
            .selected_text(current.as_deref().unwrap_or("Custom"))
            .height(480.0)
            .show_ui(ui, |ui| {
                let mut item = |ui: &mut egui::Ui, name: &str, gains: [f32; EQ_BAND_COUNT]| {
                    if ui.selectable_label(current.as_deref() == Some(name), name).clicked() {
                        picked = Some(gains);
                    }
                };
                item(ui, eq_presets::FLAT.name, eq_presets::FLAT.gains_db);
                ui.separator();
                ui.weak("Music");
                for preset in eq_presets::MUSIC {
                    item(ui, preset.name, preset.gains_db);
                }
                ui.separator();
                ui.weak("Games and voice");
                for preset in eq_presets::GENERAL {
                    item(ui, preset.name, preset.gains_db);
                }
                if !presets.saved.is_empty() {
                    ui.separator();
                    ui.weak("Yours");
                    for preset in &presets.saved {
                        item(ui, &preset.name, preset.gains_db);
                    }
                }
            });
        if presets.naming.is_none() && ui.button("Save as...").clicked() {
            presets.naming = Some(String::new());
            presets.focus_name = true;
        }
        let yours = current.as_ref().and_then(|name| presets.saved.iter().position(|p| &p.name == name));
        if let Some(index) = yours {
            if ui.button("Delete").clicked() {
                presets.saved.remove(index);
                presets.store();
            }
        }
    });
    if let Some(gains) = picked {
        for (band, gain) in gains.into_iter().enumerate() {
            mixer.set_bus_eq_band(bus_name, band, gain);
        }
    }
    if let Some(mut typed) = presets.naming.take() {
        let mut done = false;
        ui.horizontal(|ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut typed).hint_text("Preset name").desired_width(160.0));
            if std::mem::take(&mut presets.focus_name) {
                field.request_focus();
            }
            let name = typed.trim().to_string();
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!name.is_empty(), egui::Button::new("Save")).clicked() || entered) && !name.is_empty() {
                // Saving under an existing name replaces that preset.
                presets.saved.retain(|p| p.name != name);
                presets.saved.push(common::NamedEq { name, gains_db });
                presets.store();
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
        if !done {
            presets.naming = Some(typed);
        }
    }
    if let Some(error) = &presets.error {
        ui.colored_label(egui::Color32::from_rgb(230, 120, 90), error);
    }
}

fn eq_controls(ui: &mut egui::Ui, mixer: &Mixer, bus_name: &str, gains_db: [f32; EQ_BAND_COUNT]) {
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
}

/// A device menu (playback devices for a bus, or microphones and line-ins
/// for the Mic channel) showing `current` (`None`: `none_label`). Returns the
/// device just picked, `Some(None)` being the system default.
fn device_choice(
    ui: &mut egui::Ui,
    id_salt: &str,
    devices: &[AudioDevice],
    current: Option<&str>,
    none_label: &str,
) -> Option<Option<String>> {
    let current_name = match current {
        Some(id) => devices.iter().find(|d| d.id == id).map_or("(disconnected device)", |d| d.name.as_str()),
        None => none_label,
    };
    let mut picked = None;
    egui::ComboBox::from_id_salt(id_salt).selected_text(current_name).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_none(), "System Default").clicked() {
            picked = Some(None);
        }
        for device in devices {
            if ui.selectable_label(current == Some(device.id.as_str()), &device.name).clicked() {
                picked = Some(Some(device.id.clone()));
            }
        }
    });
    picked
}

impl eframe::App for MixerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Closing the window hides it to the tray: routing and clip
        // recording keep running. The tray's Quit exits.
        if ui.ctx().input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::Relaxed) {
            if let Some(tray) = &self.tray {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
                tray::hide_main_window();
                if !self.tray_hint_shown {
                    tray.notify(
                        "sonar-rs is still running",
                        "It keeps routing audio and recording clips. Right-click its tray icon to quit.",
                    );
                    self.tray_hint_shown = true;
                }
            }
        }
        self.refresh_sessions();
        self.refresh_devices();
        if self.last_route_recovery.elapsed() >= ROUTE_RECOVERY_INTERVAL {
            self.last_route_recovery = Instant::now();
            self.mixer.recover_routed_volumes();
        }

        // Levels are peaks since the previous read: read each once a frame.
        let buses = self.mixer.buses();
        let sources = self.mixer.sources();
        let master_level = self.mixer.master_level();
        self.persistence.tick(&self.mixer, &buses, &sources, &self.available_sessions);
        self.moments.tick();

        if self.window.is_none() {
            self.window = tray::main_window();
        }
        if self.start_hidden_frames > 0 && self.tray.is_some() {
            if let Some(window) = self.window {
                tray::hide_window(window);
            }
            self.start_hidden_frames -= 1;
            ui.ctx().request_repaint();
            return;
        }
        if self.window.is_some_and(|window| !tray::is_shown(window)) {
            ui.ctx().request_repaint_after(HIDDEN_WAKE_INTERVAL);
            return;
        }

        let meters_active = master_level > METER_ACTIVITY_THRESHOLD
            || buses.iter().any(|b| b.level > METER_ACTIVITY_THRESHOLD)
            || sources.iter().any(|s| s.level > METER_ACTIVITY_THRESHOLD);
        ui.ctx().request_repaint_after(if meters_active {
            ACTIVE_REPAINT_INTERVAL
        } else {
            IDLE_REPAINT_INTERVAL
        });

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Mixer, "Mixer");
                ui.selectable_value(&mut self.tab, Tab::Clips, "Clips");
            });
            if !self.config_notices.is_empty() {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(230, 170, 60), self.config_notices.join(" "));
                    if ui.small_button("Dismiss").clicked() {
                        self.config_notices.clear();
                    }
                });
            }
        });
        if self.tab == Tab::Clips {
            self.moments.tab(ui, &self.mixer);
            return;
        }

        let mic_already_added = sources.iter().any(|s| s.pid.is_none());
        let active_pids: HashSet<u32> = sources.iter().filter_map(|s| s.pid).collect();

        // --- Detail panel for the selected channel (device, EQ, app routing) ---
        if let Some(selected) = self.selected_bus.clone() {
            egui::Panel::right("bus_detail").min_size(380.0).show(ui, |ui| {
                ui.heading(&selected);
                if let Some(status) = &self.status {
                    ui.colored_label(egui::Color32::from_rgb(230, 100, 100), status);
                }
                ui.separator();

                let Some(bus) = buses.iter().find(|b| b.name == selected) else {
                    return;
                };

                if selected == "Mic" {
                    // The Mic channel's device is the microphone it records.
                    let mic = sources.iter().find(|s| s.pid.is_none());
                    ui.horizontal(|ui| {
                        ui.strong("Input device");
                        let current = mic.and_then(|m| m.input_device.as_deref());
                        let none_label = if mic.is_some() { "System Default" } else { "Not added" };
                        if let Some(picked) = device_choice(ui, "input-device", &self.input_devices, current, none_label) {
                            let result = match mic {
                                Some(mic) => self.mixer.set_mic_device(mic.id, picked),
                                None => self.mixer.add_mic_source("Microphone", "Mic", picked),
                            };
                            self.status = result.err().map(|e| format!("Couldn't open that microphone: {}", e.message()));
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.strong("Output device");
                        let current = bus.device_id.as_deref();
                        if let Some(picked) = device_choice(ui, "output-device", &self.output_devices, current, "System Default") {
                            self.mixer.set_bus_device(&selected, picked);
                        }
                    });
                    if bus.device_id.is_some() {
                        ui.horizontal(|ui| {
                            ui.label("If it disconnects");
                            let mut fallback = bus.fallback_to_default;
                            egui::ComboBox::from_id_salt("disconnect-choice")
                                .selected_text(if fallback { "Play on default output" } else { "Go silent" })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut fallback, true, "Play on default output");
                                    ui.selectable_value(&mut fallback, false, "Go silent");
                                });
                            if fallback != bus.fallback_to_default {
                                self.mixer.set_bus_fallback(&selected, fallback);
                            }
                        });
                    }
                }
                ui.separator();

                egui::CollapsingHeader::new("EQ").default_open(true).show(ui, |ui| {
                    eq_preset_controls(ui, &self.mixer, &selected, bus.eq_gains_db, &mut self.eq_presets);
                    eq_controls(ui, &self.mixer, &selected, bus.eq_gains_db);
                });
                ui.separator();

                if selected == "Mic" && !mic_already_added {
                    if ui.button("Add Microphone").clicked() {
                        let result = self.mixer.add_mic_source("Microphone", "Mic", None);
                        self.status = result.err().map(|e| format!("Couldn't open the microphone: {}", e.message()));
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
                        level_meter(ui, ("source", source.id), source.level);
                        if ui.button("Remove").clicked() {
                            self.mixer.remove_source(source.id);
                        }
                    });
                }
                let waiting: Vec<String> = self.persistence.waiting_apps(&selected).map(str::to_string).collect();
                for app in waiting {
                    ui.horizontal(|ui| {
                        ui.label(&app);
                        ui.weak("not running · routed when it starts");
                        if ui.button("Remove").clicked() {
                            self.persistence.forget_waiting_app(&selected, &app);
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
                level_meter(ui, "master", master_level);
            });
            ui.add_space(24.0);

            ui.horizontal(|ui| {
                for (i, &bus_name) in FIXED_BUSES.iter().enumerate() {
                    let bus = buses.iter().find(|b| b.name == bus_name);
                    let mut gain = bus.map(|b| b.gain).unwrap_or(1.0);
                    let level = bus.map(|b| b.level).unwrap_or(0.0);
                    let color = self.persistence.bus_color(bus_name).unwrap_or(BUS_COLORS[i % BUS_COLORS.len()]);

                    ui.vertical(|ui| {
                        ui.set_width(70.0);
                        egui::Frame::new().fill(color.gamma_multiply(0.35)).inner_margin(6.0).show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                // Vertical sliders already put the start of the
                                // range at the bottom: louder is up.
                                let slider = egui::Slider::new(&mut gain, 0.0..=2.0)
                                    .vertical()
                                    .show_value(false);
                                if ui.add_sized([36.0, 180.0], slider).changed() {
                                    self.mixer.set_bus_gain(bus_name, gain);
                                }
                                level_meter(ui, ("bus", bus_name), level);
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persistence.save_now(&self.mixer);
    }
}

fn main() -> eframe::Result<()> {
    let Some(_instance) = single_instance::acquire() else {
        return Ok(());
    };
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("bundled icon.png is a valid PNG");
    // Started at sign-in: stay in the tray until opened.
    let background = std::env::args().any(|arg| arg == autostart::BACKGROUND_ARG);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_visible(!background)
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        single_instance::WINDOW_TITLE,
        options,
        Box::new(|cc| {
            let app = MixerApp::new(&cc.egui_ctx, background).expect("failed to start audio mixer");
            Ok(Box::new(app))
        }),
    )
}
