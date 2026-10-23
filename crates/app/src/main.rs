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
use common::Priority;
use eframe::egui;

mod appearance;
mod autostart;
mod clips;
mod eq_presets;
mod games;
mod guide;
mod hotkey;
mod icons;
mod meter;
mod moments;
mod noise;
mod rename;
mod setup;
mod single_instance;
mod sounds;
mod strip;
mod theme;
mod tray;

use meter::level_meter;
use strip::Strip;
use moments::Moments;
use setup::MixerPersistence;
use tray::{Tray, TrayCommand};

/// The five channels always shown as faders, matching SteelSeries Sonar's
/// own channel naming plus a dedicated Mic channel.
const FIXED_BUSES: &[&str] = &["Game", "Chat", "Aux", "Media", "Mic"];
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    colors_window: appearance::ColorsWindow,
    guide: guide::Guide,
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
        // The Mic channel sends your processed voice to other apps through
        // a virtual cable; until you pick one it goes nowhere, since
        // hearing your own mic throws you off while talking (the saved
        // setting, if any, wins).
        mixer.set_bus_send("Mic", true);
        mixer.set_bus_heard("Mic", false);
        // Late sound is noticeable in games and voice; music and video can
        // take more delay for fewer dropouts.
        for bus in ["Game", "Chat", "Mic"] {
            mixer.set_bus_priority(bus, Priority::High);
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
            colors_window: appearance::ColorsWindow::default(),
            guide: {
                // The tour shows once, the first time Sonora opens.
                let mut guide = guide::Guide::default();
                if !common::load_presets().guide.seen {
                    guide.start();
                }
                guide
            },
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

    fn channel_color(&self, bus: &str) -> egui::Color32 {
        let index = FIXED_BUSES.iter().position(|b| *b == bus).unwrap_or(0);
        theme::channel_color(index)
    }

    /// Everything in a channel's settings panel, in sections.
    #[allow(clippy::too_many_arguments)]
    fn channel_settings(
        &mut self,
        ui: &mut egui::Ui,
        selected: &str,
        bus: &audio_engine::BusInfo,
        sources: &[audio_engine::SourceInfo],
        mic_already_added: bool,
        active_pids: &HashSet<u32>,
        color: egui::Color32,
    ) {
        if selected == "Mic" {
            theme::section(ui, "Microphone");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                // The Mic channel's device is the microphone it records.
                let mic = sources.iter().find(|s| s.pid.is_none());
                ui.horizontal(|ui| {
                    ui.label("Microphone");
                    let current = mic.and_then(|m| m.input_device.as_deref());
                    let none_label = if mic.is_some() { "System default" } else { "Not added" };
                    if let Some(picked) = device_choice(ui, "input-device", &self.input_devices, current, none_label) {
                        let result = match mic {
                            Some(mic) => self.mixer.set_mic_device(mic.id, picked),
                            None => self.mixer.add_mic_source("Microphone", "Mic", picked),
                        };
                        self.status = result.err().map(|e| format!("Couldn't open that microphone: {}", e.message()));
                    }
                });
                if !mic_already_added && ui.add(theme::primary_button("Add my microphone")).clicked() {
                    let result = self.mixer.add_mic_source("Microphone", "Mic", None);
                    self.status = result.err().map(|e| format!("Couldn't open the microphone: {}", e.message()));
                }
                self.voice_send_ui(ui, selected, bus);
                let mut monitor = bus.monitor;
                if ui
                    .checkbox(&mut monitor, "Listen to my mic")
                    .on_hover_text(
                        "Plays your voice back to you, with its EQ and noise removal, to check how you sound. \
                         Only you hear this copy. Turn it off when you're done.",
                    )
                    .changed()
                {
                    self.mixer.set_bus_monitor(selected, monitor);
                }
            });
            theme::section(ui, "Noise removal");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                noise::noise_controls(ui, self.mixer.mic_cleanup(), mic_already_added);
            });
        } else {
            theme::section(ui, "Output");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label("Plays on");
                    let current = bus.device_id.as_deref();
                    if let Some(picked) = device_choice(ui, "output-device", &self.output_devices, current, "System default") {
                        self.mixer.set_bus_device(selected, picked);
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
                            self.mixer.set_bus_fallback(selected, fallback);
                        }
                    });
                }
                self.priority_row(ui, selected, bus);
            });
        }
        if selected == "Mic" {
            theme::section(ui, "Delay");
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.priority_row(ui, selected, bus);
            });
        }

        theme::section(ui, "EQ");
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            eq_preset_controls(ui, &self.mixer, selected, bus.eq_gains_db, &mut self.eq_presets);
            eq_controls(ui, &self.mixer, selected, bus.eq_gains_db);
        });

        if selected == "Mic" {
            return;
        }
        theme::section(ui, "Apps in this channel");
        let mine: Vec<&audio_engine::SourceInfo> = sources.iter().filter(|s| s.bus == selected && s.pid.is_some()).collect();
        // A saved app with one copy already here (a second Discord, say)
        // isn't listed again as waiting.
        let here: Vec<String> = mine
            .iter()
            .filter_map(|s| audio_engine::process_executable_name(s.pid?))
            .map(|exe| exe.to_lowercase())
            .collect();
        let waiting: Vec<String> = self
            .persistence
            .waiting_apps(selected)
            .filter(|app| !here.iter().any(|exe| exe == app))
            .map(str::to_string)
            .collect();
        if mine.is_empty() && waiting.is_empty() {
            ui.label(egui::RichText::new("None yet. Add one below.").color(theme::text_muted()));
        }
        for source in mine {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(app_label(&source.name)).strong())
                        .on_hover_text(format!("{} (process {})", source.name, source.pid.unwrap_or_default()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("Remove").small()).clicked() {
                            self.mixer.remove_source(source.id);
                        }
                        level_meter(ui, ("source", source.id), source.level, color);
                    });
                });
                ui.horizontal(|ui| {
                    let mut muted = source.muted;
                    if ui.checkbox(&mut muted, "Mute").changed() {
                        self.mixer.set_source_muted(source.id, muted);
                    }
                    let mut gain = source.gain;
                    if ui
                        .add(egui::Slider::new(&mut gain, 0.0..=2.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)))
                        .changed()
                    {
                        self.mixer.set_source_gain(source.id, gain);
                    }
                });
            });
        }
        for app in waiting {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(capitalized(app_label(&app))).strong());
                    ui.label(egui::RichText::new("joins when it starts").small().color(theme::text_muted()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("Remove").small()).clicked() {
                            self.persistence.forget_waiting_app(selected, &app);
                        }
                    });
                });
            });
        }

        theme::section(ui, "Add an app");
        let addable: Vec<&AvailableSession> =
            self.available_sessions.iter().filter(|s| !active_pids.contains(&s.pid)).collect();
        if addable.is_empty() {
            ui.label(egui::RichText::new("No other apps are playing sound right now.").color(theme::text_muted()));
        }
        let mut add = None;
        for session in addable {
            ui.horizontal(|ui| {
                ui.label(app_label(&session.name)).on_hover_text(format!("{} (process {})", session.name, session.pid));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Add").small()).clicked() {
                        add = Some((session.pid, session.name.clone()));
                    }
                });
            });
        }
        if let Some((pid, name)) = add {
            if let Err(error) = self.mixer.add_source(pid, name, selected.to_string()) {
                self.status = Some(format!("Couldn't add that app: {}", error.message()));
            }
        }
    }

    fn priority_row(&mut self, ui: &mut egui::Ui, selected: &str, bus: &audio_engine::BusInfo) {
        ui.horizontal(|ui| {
            ui.label("Priority");
            let mut priority = bus.priority;
            let label = |p: Priority| match p {
                Priority::High => "High (least delay)",
                Priority::Normal => "Normal",
                Priority::Low => "Low (smoothest)",
            };
            egui::ComboBox::from_id_salt("priority-choice")
                .selected_text(label(priority))
                .show_ui(ui, |ui| {
                    for p in [Priority::High, Priority::Normal, Priority::Low] {
                        ui.selectable_value(&mut priority, p, label(p));
                    }
                })
                .response
                .on_hover_text(
                    "How much delay this channel may have. High keeps it as small as it can (for games \
                     and voice); Low leaves room so it never crackles when your PC is busy (for music \
                     and video). If a High channel crackles, try Normal.",
                );
            if priority != bus.priority {
                self.mixer.set_bus_priority(selected, priority);
            }
            ui.label(egui::RichText::new(format!("~{:.0} ms", bus.delay_ms)).color(theme::text_muted())).on_hover_text(
                "Sonora's own delay for this channel. For apps, Windows adds about 45 ms more \
                 before Sonora gets their sound (measured on this PC).",
            );
        });
    }

    /// Where the Mic channel sends your processed voice: a virtual cable
    /// that Discord and games then use as a microphone, or nowhere.
    fn voice_send_ui(&mut self, ui: &mut egui::Ui, bus_name: &str, bus: &audio_engine::BusInfo) {
        let cable = self.output_devices.iter().find(|d| is_virtual_cable(&d.name)).cloned();
        let sending_to = bus.heard.then(|| bus.device_id.clone());
        let label = match &sending_to {
            None => "Nowhere".to_string(),
            Some(None) => "Default output (you'll hear yourself)".to_string(),
            Some(Some(id)) => self
                .output_devices
                .iter()
                .find(|d| &d.id == id)
                .map_or("(disconnected device)".to_string(), |d| d.name.clone()),
        };
        let mut picked: Option<Option<String>> = None;
        ui.horizontal(|ui| {
            ui.strong("Send my voice to");
            egui::ComboBox::from_id_salt("voice-send").selected_text(label).width(260.0).show_ui(ui, |ui| {
                if ui.selectable_label(sending_to.is_none(), "Nowhere").clicked() {
                    picked = Some(None);
                }
                // The cable first: it's what this is for.
                let mut devices: Vec<&AudioDevice> = self.output_devices.iter().collect();
                devices.sort_by_key(|d| !is_virtual_cable(&d.name));
                for device in devices {
                    let selected = sending_to.as_ref().and_then(|s| s.as_deref()) == Some(device.id.as_str());
                    if ui.selectable_label(selected, &device.name).clicked() {
                        picked = Some(Some(device.id.clone()));
                    }
                }
            });
        });
        if let Some(choice) = picked {
            self.mixer.set_bus_heard(bus_name, choice.is_some());
            self.mixer.set_bus_device(bus_name, choice);
        }
        let to_cable = matches!(&sending_to, Some(Some(id)) if cable.as_ref().is_some_and(|c| &c.id == id));
        match (&cable, to_cable) {
            (Some(_), true) => {
                ui.weak("In Discord and games, pick \"CABLE Output\" as your microphone.");
            }
            (Some(cable), false) => {
                ui.horizontal(|ui| {
                    ui.weak("Virtual cable found.");
                    if ui.button("Send my voice to it").clicked() {
                        self.mixer.set_bus_heard(bus_name, true);
                        self.mixer.set_bus_device(bus_name, Some(cable.id.clone()));
                    }
                });
            }
            (None, _) => {
                ui.horizontal_wrapped(|ui| {
                    ui.weak(
                        "To use your cleaned-up voice in Discord and games, install the free VB-Audio Virtual \
                         Cable, then pick it here.",
                    );
                    if ui.small_button("Get it").clicked() {
                        clips::open_url("https://vb-audio.com/Cable/");
                    }
                });
            }
        }
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
                for category in eq_presets::categories_for(bus_name) {
                    ui.separator();
                    ui.weak(category.name);
                    for preset in category.presets {
                        item(ui, preset.name, preset.gains_db);
                    }
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
        ui.colored_label(theme::danger(), error);
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

/// `name` with its first letter in capitals ("discord" -> "Discord"), for
/// names that come from lowercase executable names.
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// An app's name for showing: its executable name without ".exe".
fn app_label(name: &str) -> &str {
    let len = name.len();
    if len > 4 && name[len - 4..].eq_ignore_ascii_case(".exe") {
        &name[..len - 4]
    } else {
        name
    }
}

/// Whether an output device is a virtual cable whose other end apps can
/// record from (VB-Audio Virtual Cable's input side).
fn is_virtual_cable(name: &str) -> bool {
    name.contains("CABLE Input") || name.contains("VB-Audio Virtual Cable")
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
                        "Sonora is still running",
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

        // The guide moves the app to what each step is about.
        match self.guide.view() {
            Some(guide::View::Mixer(panel)) => {
                self.tab = Tab::Mixer;
                self.selected_bus = panel.map(str::to_string);
            }
            Some(guide::View::Clips) => self.tab = Tab::Clips,
            None => {}
        }
        self.colors_window.show(ui.ctx());
        if let Some(guide::Outcome::Done) = self.guide.show(ui.ctx(), &self.moments.hotkey_label()) {
            self.tab = Tab::Mixer;
            self.selected_bus = None;
            if let Err(error) = common::update_presets(|presets| presets.guide.seen = true) {
                self.config_notices.push(format!("Couldn't save that you've seen the guide: {error}"));
            }
        }

        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(18, 10)))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Sonora").size(21.0).strong().color(theme::text()));
                    ui.label(egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).small().color(theme::text_muted()));
                    ui.add_space(18.0);
                    for (tab, label) in [(Tab::Mixer, "Mixer"), (Tab::Clips, "Clips")] {
                        let selected = self.tab == tab;
                        let text = egui::RichText::new(label).size(14.5);
                        let button = if selected {
                            theme::primary_button(label).min_size(egui::vec2(76.0, 30.0))
                        } else {
                            egui::Button::new(text.color(theme::text_muted())).frame(false).min_size(egui::vec2(76.0, 30.0))
                        };
                        let response = ui.add(button);
                        if response.clicked() {
                            self.tab = tab;
                        }
                        if tab == Tab::Clips {
                            guide::target(ui.ctx(), "clips-tab", response.rect);
                        }
                    }
                    ui.add_space(6.0);
                    let menu = ui.menu_button(egui::RichText::new("Menu").size(14.5).color(theme::text_muted()), |ui| {
                        if ui.button("Colors…").clicked() {
                            self.colors_window.open();
                            ui.close();
                        }
                        if ui.button("Getting started guide").clicked() {
                            self.guide.start();
                            ui.close();
                        }
                    });
                    guide::target(ui.ctx(), "menu", menu.response.rect);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        level_meter(ui, "master", master_level, theme::accent());
                        let mut gain = self.mixer.master_gain();
                        ui.label(egui::RichText::new(format!("{:.0}%", gain * 100.0)).color(theme::text_muted()));
                        if ui
                            .add(egui::Slider::new(&mut gain, 0.0..=2.0).show_value(false))
                            .on_hover_text("Everything you hear. Doesn't change your voice sent to Discord and games.")
                            .changed()
                        {
                            self.mixer.set_master_gain(gain);
                        }
                        ui.label(egui::RichText::new("Master").strong());
                        guide::target(ui.ctx(), "master", ui.min_rect());
                    });
                });
                if !self.config_notices.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(theme::warning(), self.config_notices.join(" "));
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

        // --- Settings for the selected channel (device, EQ, apps) ---
        if let Some(selected) = self.selected_bus.clone() {
            let color = self.channel_color(&selected);
            let panel = egui::Panel::right("bus_detail")
                .min_size(400.0)
                .default_size(430.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::surface())
                        .inner_margin(egui::Margin::same(16))
                        .stroke(egui::Stroke::new(1.0, theme::border())),
                )
                .show(ui, |ui| {
                    let Some(bus) = buses.iter().find(|b| b.name == selected) else {
                        return;
                    };
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 6.0, color);
                        ui.heading(&selected);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("✕").frame(false)).on_hover_text("Close").clicked() {
                                self.selected_bus = None;
                            }
                        });
                    });
                    if let Some(status) = &self.status {
                        ui.colored_label(theme::danger(), status);
                    }
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        self.channel_settings(ui, &selected, bus, &sources, mic_already_added, &active_pids, color);
                    });
                });
            guide::target(ui.ctx(), "channel-panel", panel.response.rect);
            if self.selected_bus.is_none() {
                ui.ctx().request_repaint();
            }
        }

        // --- Channel strips ---
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::same(20)))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    let strips = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        for &bus_name in FIXED_BUSES {
                            let bus = buses.iter().find(|b| b.name == bus_name);
                            let app_count = sources.iter().filter(|s| s.bus == bus_name && s.pid.is_some()).count();
                            let detail = match bus_name {
                                "Mic" if !mic_already_added => "No mic added".to_string(),
                                "Mic" if bus.is_some_and(|b| b.heard) => "Voice sent".to_string(),
                                "Mic" => "Voice not sent".to_string(),
                                _ => match app_count {
                                    0 => "No apps".to_string(),
                                    1 => "1 app".to_string(),
                                    n => format!("{n} apps"),
                                },
                            };
                            let response = Strip {
                                name: bus_name,
                                color: self.channel_color(bus_name),
                                gain: bus.map_or(1.0, |b| b.gain),
                                muted: bus.is_some_and(|b| b.muted),
                                peak: bus.map_or(0.0, |b| b.level),
                                detail,
                                selected: self.selected_bus.as_deref() == Some(bus_name),
                            }
                            .show(ui);
                            if response.selected {
                                self.selected_bus = Some(bus_name.to_string());
                            }
                            if let Some(gain) = response.gain {
                                self.mixer.set_bus_gain(bus_name, gain);
                            }
                            if response.toggle_mute {
                                self.mixer.set_bus_muted(bus_name, !bus.is_some_and(|b| b.muted));
                            }
                        }
                    });
                    guide::target(ui.ctx(), "strips", strips.response.rect);
                    if self.selected_bus.is_none() {
                        ui.add_space(16.0);
                        ui.label(
                            egui::RichText::new("Click a channel to choose where it plays, its EQ, and which apps go in it.")
                                .color(theme::text_muted()),
                        );
                    }
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persistence.save_now(&self.mixer);
        self.colors_window.save_now();
    }
}

fn main() -> eframe::Result<()> {
    let Some(_instance) = single_instance::acquire() else {
        return Ok(());
    };
    let rename_notes = rename::move_from_sonar_rs();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("bundled icon.png is a valid PNG");
    // Started at sign-in: stay in the tray until opened.
    let background = std::env::args().any(|arg| arg == autostart::BACKGROUND_ARG);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([760.0, 560.0])
            .with_visible(!background)
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        single_instance::WINDOW_TITLE,
        options,
        Box::new(|cc| {
            theme::load(&cc.egui_ctx);
            let mut app = MixerApp::new(&cc.egui_ctx, background).expect("failed to start audio mixer");
            app.config_notices.extend(rename_notes);
            Ok(Box::new(app))
        }),
    )
}
