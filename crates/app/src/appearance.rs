//! The Colors window: pick a preset, change any color Sonora draws with,
//! and share your colors as text. Changes show at once and are saved
//! shortly after you stop changing them.

use std::time::{Duration, Instant};

use common::{Rgb, ThemeColors};
use eframe::egui;

use crate::{theme, FIXED_BUSES};

/// How long after the last change colors are saved, so dragging around a
/// color picker doesn't write the settings file every frame.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// Where a color is kept in [`ThemeColors`].
type Field = fn(&mut ThemeColors) -> &mut Rgb;
/// A color's name, what it colors, and where it's kept.
type ColorRow = (&'static str, &'static str, Field);

/// Each section's title and colors.
const SECTIONS: &[(&str, &[ColorRow])] = &[
    (
        "Window",
        &[
            ("Background", "Behind everything", |c| &mut c.background),
            ("Panels", "Side panels and windows", |c| &mut c.panels),
            ("Cards", "The boxes things are grouped in", |c| &mut c.cards),
        ],
    ),
    (
        "Controls",
        &[
            ("Buttons and fields", "Buttons, menus, text fields and slider rails", |c| &mut c.controls),
            ("Hovered", "A control under the mouse", |c| &mut c.controls_hovered),
            ("Borders", "Outlines of cards and controls", |c| &mut c.borders),
        ],
    ),
    (
        "Text",
        &[
            ("Text", "Most text", |c| &mut c.text),
            ("Muted text", "Hints, labels and details", |c| &mut c.text_muted),
            ("Text on accent", "Text and icons on accent buttons", |c| &mut c.text_on_accent),
        ],
    ),
    (
        "Highlights",
        &[
            ("Accent", "Selected tab, main buttons, sliders' filled part", |c| &mut c.accent),
            ("Warning", "Things to check", |c| &mut c.warning),
            ("Error", "Errors and muted channels", |c| &mut c.error),
        ],
    ),
    ("Clips", &[("Desktop audio track", "The clip editor's everything-you-heard track", |c| &mut c.desktop_track)]),
];

#[derive(Default)]
pub struct ColorsWindow {
    open: bool,
    /// The colors when the window was opened, for Undo.
    opened_with: Option<ThemeColors>,
    /// Colors pasted to apply.
    pasted: String,
    message: Option<(String, bool)>,
    changed_at: Option<Instant>,
}

impl ColorsWindow {
    pub fn open(&mut self) {
        self.open = true;
        self.opened_with = Some(theme::colors());
        self.message = None;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if self.changed_at.is_some_and(|at| at.elapsed() >= SAVE_DELAY) {
            self.save_now();
        } else if self.changed_at.is_some() {
            ctx.request_repaint_after(SAVE_DELAY);
        }
        if !self.open {
            return;
        }
        let mut open = true;
        let mut colors = theme::colors();
        egui::Window::new("Colors")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .default_pos(ctx.content_rect().center() - egui::vec2(220.0, 300.0))
            .show(ctx, |ui| {
                // Tall enough to see most colors at once, on any window size.
                let height = (ctx.content_rect().height() - 180.0).clamp(160.0, 640.0);
                egui::ScrollArea::vertical().min_scrolled_height(height).max_height(height).show(ui, |ui| {
                    self.contents(ui, &mut colors);
                });
            });
        if colors != theme::colors() {
            theme::set(ctx, colors);
            self.changed_at = Some(Instant::now());
        }
        if !open {
            self.open = false;
            self.save_now();
        }
    }

    /// Saves changed colors now (closing the window, quitting).
    pub fn save_now(&mut self) {
        if self.changed_at.take().is_none() {
            return;
        }
        let colors = theme::colors();
        if let Err(error) = common::update_presets(|presets| presets.colors = Some(colors)) {
            self.message = Some((format!("Couldn't save your colors: {error}"), true));
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, colors: &mut ThemeColors) {
        ui.horizontal(|ui| {
            ui.label("Start from");
            let current = theme::preset_name(colors).unwrap_or("Your colors");
            egui::ComboBox::from_id_salt("color-preset").selected_text(current).show_ui(ui, |ui| {
                for (name, preset) in theme::PRESETS {
                    if ui.selectable_label(preset == colors, *name).clicked() {
                        *colors = *preset;
                    }
                }
            });
            let undo = self.opened_with.filter(|before| before != colors);
            if ui.add_enabled(undo.is_some(), egui::Button::new("Undo")).on_hover_text("Back to the colors you had when you opened this").clicked() {
                if let Some(before) = undo {
                    *colors = before;
                }
            }
        });
        ui.label(egui::RichText::new("Changes show right away and are saved automatically.").small().color(theme::text_muted()));

        for (title, fields) in SECTIONS {
            theme::section(ui, title);
            for (label, hint, field) in *fields {
                color_row(ui, label, hint, field(colors));
            }
            if *title == "Highlights" {
                theme::section(ui, "Channels");
                for (bus, channel) in FIXED_BUSES.iter().zip(colors.channels.iter_mut()) {
                    color_row(ui, bus, "The channel's strip, meters and clip track", channel);
                }
            }
        }

        theme::section(ui, "Share");
        ui.horizontal(|ui| {
            if ui.button("Copy my colors").on_hover_text("Copies them as text to send to someone").clicked() {
                if let Ok(json) = serde_json::to_string_pretty(colors) {
                    ui.ctx().copy_text(json);
                    self.message = Some(("Copied. Anyone can paste them here.".into(), false));
                }
            }
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.pasted)
                .hint_text("Paste colors someone shared")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        if ui.add_enabled(!self.pasted.trim().is_empty(), egui::Button::new("Use pasted colors")).clicked() {
            match serde_json::from_str::<ThemeColors>(self.pasted.trim()) {
                Ok(pasted) => {
                    *colors = pasted;
                    self.pasted.clear();
                    self.message = Some(("Pasted colors in use.".into(), false));
                }
                Err(error) => self.message = Some((format!("Those aren't Sonora colors: {error}"), true)),
            }
        }
        if let Some((text, is_error)) = &self.message {
            ui.colored_label(if *is_error { theme::danger() } else { theme::text_muted() }, text);
        }
    }
}

/// One color: its name, a swatch that opens a picker, and its `#RRGGBB`.
fn color_row(ui: &mut egui::Ui, label: &str, hint: &str, rgb: &mut Rgb) {
    ui.horizontal(|ui| {
        let mut color = theme::color(*rgb);
        if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque).changed() {
            *rgb = theme::rgb(color);
        }
        // While typing, the field keeps your text; a valid color applies.
        let id = ui.make_persistent_id(("hex", label));
        let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| rgb.to_hex());
        let field = ui.add(egui::TextEdit::singleline(&mut text).desired_width(70.0).font(egui::TextStyle::Monospace));
        if field.changed() {
            if let Some(typed) = Rgb::from_hex(&text) {
                *rgb = typed;
            }
        }
        if field.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, text));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        ui.label(label).on_hover_text(hint);
    });
}
