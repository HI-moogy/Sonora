//! Sonora's look: one dark theme, a warm coral accent, and a color per
//! channel. Every screen takes its colors and spacing from here.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

/// Window background.
pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x19);
/// Panels and cards.
pub const SURFACE: Color32 = Color32::from_rgb(0x19, 0x1C, 0x23);
/// Controls and raised parts of cards.
pub const RAISED: Color32 = Color32::from_rgb(0x22, 0x26, 0x2F);
/// Cards: darker than controls, so sliders' rails and fields stand out.
pub const CARD: Color32 = Color32::from_rgb(0x15, 0x18, 0x1F);
/// Hovered controls.
pub const RAISED_HOVER: Color32 = Color32::from_rgb(0x2B, 0x30, 0x3B);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x31, 0x3C);
pub const TEXT: Color32 = Color32::from_rgb(0xE7, 0xE9, 0xEE);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8C, 0x93, 0xA3);
/// Sonora's accent: selected tabs, primary buttons, focus.
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x59);
pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x6D, 0x6D);
pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x4C);

/// The channels' own colors: Game, Chat, Aux, Media, Mic.
pub const CHANNEL_COLORS: [Color32; 5] = [
    Color32::from_rgb(0x4F, 0xA3, 0xFF),
    Color32::from_rgb(0x45, 0xD0, 0x8A),
    Color32::from_rgb(0xA7, 0x8B, 0xFA),
    Color32::from_rgb(0xF4, 0x72, 0xB6),
    Color32::from_rgb(0xFB, 0xBF, 0x24),
];

pub const RADIUS: u8 = 8;

pub fn apply(ctx: &egui::Context) {
    // Sonora has one (dark) look, whatever Windows' app mode is.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(19.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.slider_width = 150.0;
    style.spacing.combo_width = 180.0;
    style.spacing.window_margin = Margin::same(12);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.extreme_bg_color = BG;
    v.faint_bg_color = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(RADIUS);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    v.selection.bg_fill = ACCENT.linear_multiply(0.85);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = DANGER;
    v.override_text_color = None;
    // Sliders fill up to their handle, so a value reads at a glance.
    v.slider_trailing_fill = true;

    let radius = CornerRadius::same(RADIUS - 2);
    let widgets = &mut v.widgets;
    widgets.noninteractive.bg_fill = SURFACE;
    widgets.noninteractive.weak_bg_fill = SURFACE;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
    widgets.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut widgets.inactive, RAISED),
        (&mut widgets.hovered, RAISED_HOVER),
        (&mut widgets.active, RAISED_HOVER),
        (&mut widgets.open, RAISED_HOVER),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.bg_stroke = Stroke::new(1.0, BORDER);
        state.fg_stroke = Stroke::new(1.0, TEXT);
        state.expansion = 0.0;
    }
    widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT_MUTED);
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// The color of channel `index` (in [`crate::FIXED_BUSES`] order).
pub fn channel_color(index: usize) -> Color32 {
    CHANNEL_COLORS[index % CHANNEL_COLORS.len()]
}

/// A card: the frame every grouped part of a screen sits in.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(12))
}

/// A small uppercase heading over a section of a panel.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title.to_uppercase()).small().strong().color(TEXT_MUTED));
}

/// A filled accent button, for a screen's main action.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(Color32::from_rgb(0x1A, 0x10, 0x0C)).strong()).fill(ACCENT)
}
