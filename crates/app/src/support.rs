//! Helping testers report problems: crash reports written to the settings
//! folder, a summary of the system and voice setup to paste into a bug
//! report, and the About window that offers both.

use std::path::{Path, PathBuf};
use std::process::Command;

use audio_engine::{list_input_devices, list_output_devices, AudioDevice, Mixer};
use eframe::egui;

use crate::theme;

/// The GitHub repository, from Cargo.toml's `repository` (empty until set).
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// Whether testers can open the repository's issues. It's private for now
/// (testers get Sonora as a file), so the About window doesn't send them
/// there; set this when it goes public.
const REPOSITORY_IS_PUBLIC: bool = false;

/// Sonora's own license, and those of the libraries built into it.
const LICENSE: &str = include_str!("../../../LICENSE");
const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.txt"));

/// Writes the licenses Sonora carries to the settings folder and opens
/// them in the default text viewer.
fn open_licenses() {
    let path = common::config_dir().join("licenses.txt");
    let text = format!("Sonora

{LICENSE}

{NOTICES}");
    let _ = std::fs::create_dir_all(common::config_dir());
    if std::fs::write(&path, text).is_ok() {
        open_url(&path.to_string_lossy());
    }
}

/// Where crash reports are written.
pub fn crash_dir() -> PathBuf {
    common::config_dir().join("crashes")
}

/// Writes a report of any panic (what, where, and the backtrace) to
/// [`crash_dir`], since a crash in a windowed app otherwise leaves no trace.
pub fn install_crash_reports() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let report = format!(
            "{}\nThread: {}\n{info}\n\nBacktrace:\n{}\n",
            system_summary(),
            thread.name().unwrap_or("unnamed"),
            std::backtrace::Backtrace::force_capture(),
        );
        let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let dir = crash_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("crash-{seconds}.txt")), report);
        default_hook(info);
    }));
}

/// Sonora's version and the system it runs on, for a bug report.
pub fn system_summary() -> String {
    format!("Sonora {} on {}", env!("CARGO_PKG_VERSION"), os_name())
}

/// What "Copy details" gives: [`system_summary`], and how your voice gets to
/// other apps, where most setup trouble is (a doubled, underwater-sounding
/// voice is two copies of it reaching the virtual cable).
fn details(mixer: &Mixer) -> String {
    let mut lines = vec![system_summary()];
    lines.extend(voice_details(mixer));
    lines.join("
")
}

fn voice_details(mixer: &Mixer) -> Vec<String> {
    let inputs = list_input_devices().unwrap_or_default();
    let outputs = list_output_devices().unwrap_or_default();
    let sources = mixer.source_settings();
    let Some(mic) = sources.iter().find(|s| s.pid.is_none()) else {
        return vec!["Microphone: not added".into()];
    };
    let mic_name = match &mic.input_device {
        Some(id) => device_name(&inputs, id),
        None => "system default".into(),
    };
    let mut lines = vec![format!("Microphone: {mic_name}{}", if mic.muted { ", muted" } else { "" })];

    let buses = mixer.bus_settings();
    let Some(bus) = buses.iter().find(|b| b.name == mic.bus) else { return lines };
    let send_device = bus.device_id.as_ref().filter(|_| bus.heard);
    let sends_to = match (bus.heard, send_device) {
        (false, _) => "nowhere".into(),
        (true, None) => "the default output".into(),
        (true, Some(id)) => device_name(&outputs, id),
    };
    let cleanup = mixer.mic_cleanup();
    let shaping: Vec<String> = bus
        .eq
        .iter()
        .filter(|band| band.shapes())
        .map(|band| format!("{:?} {:.0} Hz {:+.1} dB Q {:.2}", band.kind, band.freq, band.gain_db, band.q))
        .collect();
    let eq = match (bus.eq_enabled, shaping.is_empty()) {
        (false, _) => "off".into(),
        (true, true) => "flat".into(),
        (true, false) => shaping.join(", "),
    };
    lines.push(format!(
        "{} channel: sends to {sends_to}; Listen to my mic {}; noise removal {:?}{}; EQ {eq}{}",
        bus.name,
        if bus.monitor { "on" } else { "off" },
        cleanup.mode(),
        if cleanup.quiet_far_voices() { " + quiet far voices" } else { "" },
        if bus.muted { "; muted" } else { "" },
    ));
    if let Some(id) = send_device {
        let also: Vec<&str> = buses
            .iter()
            .filter(|b| b.name != bus.name && b.heard && b.device_id.as_ref() == Some(id))
            .map(|b| b.name.as_str())
            .collect();
        if !also.is_empty() {
            lines.push(format!("Also playing there: {}", also.join(", ")));
        }
    }
    #[cfg(windows)]
    lines.extend(windows_voice_details(mic.input_device.as_deref(), send_device, &inputs, &outputs));
    lines
}

/// What Windows adds: how it records the mic, its default output, and any
/// microphone it plays straight to an output ("Listen to this device").
#[cfg(windows)]
fn windows_voice_details(
    mic: Option<&str>,
    send_device: Option<&String>,
    inputs: &[AudioDevice],
    outputs: &[AudioDevice],
) -> Vec<String> {
    use audio_engine::devices::{default_input_device_id, default_output_device_id, input_format, listened_inputs};
    let mut lines = Vec::new();
    let mut recorded = String::new();
    if mic.is_none() {
        if let Ok(id) = default_input_device_id() {
            recorded = format!("{}, ", device_name(inputs, &id));
        }
    }
    match input_format(mic) {
        Ok((channels, rate)) => lines.push(format!("Windows records the mic ({recorded}{channels} ch, {rate} Hz)")),
        Err(error) => lines.push(format!("Windows records the mic ({recorded}format unknown: {})", error.message())),
    }
    if let Ok(id) = default_output_device_id() {
        let same = if send_device == Some(&id) { ", the same device the voice is sent to" } else { "" };
        lines.push(format!("Windows default output: {}{same}", device_name(outputs, &id)));
    }
    match listened_inputs() {
        Ok(listened) if listened.is_empty() => lines.push("Windows \"Listen to this device\": off for every mic".into()),
        Ok(listened) => {
            for input in listened {
                let plays_on = input.plays_on.map_or("the default output".into(), |id| device_name(outputs, &id));
                lines.push(format!("Windows \"Listen to this device\": {} plays on {plays_on}", input.name));
            }
        }
        Err(_) => lines.push("Windows \"Listen to this device\": couldn't check".into()),
    }
    lines
}

fn device_name(devices: &[AudioDevice], id: &str) -> String {
    devices.iter().find(|d| d.id == id).map_or_else(|| "a disconnected device".into(), |d| d.name.clone())
}

#[cfg(windows)]
fn os_name() -> String {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let read = |name: PCWSTR| -> Option<String> {
        let mut buffer = [0u16; 128];
        let mut size = (buffer.len() * 2) as u32;
        unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                name,
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        }
        .ok()
        .ok()?;
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buffer[..len]))
    };
    // Windows 11 still says "Windows 10" in ProductName; the build tells.
    let build = read(w!("CurrentBuildNumber")).unwrap_or_default();
    let name = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { "Windows 11".to_string() } else { "Windows 10".to_string() };
    let version = read(w!("DisplayVersion")).unwrap_or_default();
    format!("{name} {version} (build {build})")
}

#[cfg(not(windows))]
fn os_name() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distro = release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|name| name.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".into());
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "?".into());
    format!("{distro}, {} desktop, {} session", var("XDG_CURRENT_DESKTOP"), var("XDG_SESSION_TYPE"))
}

/// Opens `url` in the default browser (or a file in its default app).
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(url).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// Opens `dir` in the file manager, creating it first if need be.
pub fn open_folder(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(dir).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(dir).spawn();
}

/// The About window: the version and system, and how to report a bug.
#[derive(Default)]
pub struct AboutWindow {
    open: bool,
    copied: bool,
}

impl AboutWindow {
    pub fn open(&mut self) {
        self.open = true;
        self.copied = false;
    }

    pub fn show(&mut self, ctx: &egui::Context, mixer: &Mixer) {
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("About Sonora")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .default_pos(ctx.content_rect().center() - egui::vec2(210.0, 150.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(system_summary()).strong());
                ui.label(
                    egui::RichText::new("This is an alpha: thanks for trying it. If something goes wrong, please report it.")
                        .color(theme::text_muted()),
                );
                theme::section(ui, "Report a bug");
                ui.label(if REPOSITORY_IS_PUBLIC {
                    "Say what you did, what happened and what you expected. Paste the details below into it, and attach the newest crash report if Sonora crashed."
                } else {
                    "Tell whoever gave you Sonora what you did, what happened and what you expected. Paste the details below into your message, and attach the newest crash report if Sonora crashed."
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").on_hover_text("Sonora's version, your system, and how your voice is sent").clicked() {
                        ui.ctx().copy_text(details(mixer));
                        self.copied = true;
                    }
                    if REPOSITORY_IS_PUBLIC && !REPOSITORY.is_empty() && ui.button("Report on GitHub").clicked() {
                        ui.ctx().copy_text(details(mixer));
                        self.copied = true;
                        open_url(&format!("{REPOSITORY}/issues/new/choose"));
                    }
                    if ui.button("Crash reports").on_hover_text("Open the folder Sonora writes crash reports to").clicked() {
                        open_folder(&crash_dir());
                    }
                    if ui.button("Settings folder").clicked() {
                        open_folder(&common::config_dir());
                    }
                });
                theme::section(ui, "Licenses");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Sonora's code is MIT licensed; the Sonora name and logo aren't.");
                    if ui.button("Open-source licenses").on_hover_text("Sonora's license and those of the libraries it's built with").clicked() {
                        open_licenses();
                    }
                });
                if self.copied {
                    ui.label(egui::RichText::new("Details copied.").small().color(theme::text_muted()));
                }
            });
        self.open = open;
    }
}
