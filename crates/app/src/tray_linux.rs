//! The tray icon on Linux (a StatusNotifierItem: KDE Plasma and the Steam
//! Deck's desktop show it; GNOME needs the AppIndicator extension, which
//! Ubuntu has on), and hiding and showing the main window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::MenuItem;

use crate::autostart;

/// What the user picked from the tray icon, for the app to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    Quit,
}

/// Whether the main window is hidden to the tray.
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// The tray icon. Removed on drop.
pub struct Tray {
    handle: ksni::blocking::Handle<SonoraTray>,
}

struct SonoraTray {
    on_command: Arc<dyn Fn(TrayCommand) + Send + Sync>,
}

impl ksni::Tray for SonoraTray {
    fn id(&self) -> String {
        "sonora".into()
    }

    fn title(&self) -> String {
        "Sonora".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) else {
            return Vec::new();
        };
        // RGBA to ARGB.
        let data = icon.rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        vec![ksni::Icon { width: icon.width as i32, height: icon.height as i32, data }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: format!("Sonora {}", env!("CARGO_PKG_VERSION")), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.on_command)(TrayCommand::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Sonora".into(),
                activate: Box::new(|tray: &mut Self| (tray.on_command)(TrayCommand::Open)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start at login".into(),
                checked: autostart::is_enabled(),
                activate: Box::new(|_: &mut Self| {
                    let _ = autostart::set_enabled(!autostart::is_enabled());
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| (tray.on_command)(TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

impl Tray {
    /// Adds the icon; `on_command` runs on the tray's own thread. Fails if
    /// the desktop has nowhere to show it.
    pub fn start(on_command: impl Fn(TrayCommand) + Send + Sync + 'static) -> Result<Self, String> {
        let handle = SonoraTray { on_command: Arc::new(on_command) }.spawn().map_err(|error| error.to_string())?;
        Ok(Self { handle })
    }

    /// Notifications aren't shown from the tray on Linux.
    pub fn notify(&self, _title: &str, _text: &str) {}
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown().wait();
    }
}

/// Quitting leaves nothing to put back on Linux: routed apps return to
/// the default output as Sonora stops.
pub fn confirm_quit() -> bool {
    true
}

pub fn hide_main_window(ctx: &egui::Context) {
    HIDDEN.store(true, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}

pub fn show_main_window(ctx: &egui::Context) {
    HIDDEN.store(false, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

/// Whether the main window is shown (not hidden to the tray).
pub fn main_window_shown() -> Option<bool> {
    Some(!HIDDEN.load(Ordering::Relaxed))
}
