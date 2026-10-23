//! Sonora's look: your chosen colors (Sonora's own dark coral by default,
//! a few presets, or anything you pick) and the spacing every screen uses.
//! Screens take their colors from here, so changing them changes them
//! everywhere at once.

use std::sync::RwLock;

use common::{Rgb, ThemeColors};
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

pub const RADIUS: u8 = 8;

/// The built-in color sets, Sonora's own first.
pub const PRESETS: &[(&str, ThemeColors)] = &[
    ("Sonora", ThemeColors::SONORA),
    ("Sonora Light", LIGHT),
    ("Midnight", MIDNIGHT),
    ("Forest", FOREST),
    ("Synthwave", SYNTHWAVE),
    ("Classic", CLASSIC),
    ("High contrast", HIGH_CONTRAST),
];

const LIGHT: ThemeColors = ThemeColors {
    background: Rgb::new(0xEE, 0xF0, 0xF4),
    panels: Rgb::new(0xFA, 0xFA, 0xFC),
    cards: Rgb::new(0xFF, 0xFF, 0xFF),
    controls: Rgb::new(0xE3, 0xE6, 0xEC),
    controls_hovered: Rgb::new(0xD6, 0xDA, 0xE2),
    borders: Rgb::new(0xD0, 0xD4, 0xDD),
    text: Rgb::new(0x1B, 0x1F, 0x27),
    text_muted: Rgb::new(0x5F, 0x66, 0x75),
    accent: Rgb::new(0xE8, 0x5D, 0x3B),
    text_on_accent: Rgb::new(0xFF, 0xFF, 0xFF),
    warning: Rgb::new(0xA8, 0x6A, 0x00),
    error: Rgb::new(0xD0, 0x3B, 0x3B),
    channels: [
        Rgb::new(0x1F, 0x7A, 0xE0),
        Rgb::new(0x1E, 0x9E, 0x63),
        Rgb::new(0x7C, 0x5C, 0xE0),
        Rgb::new(0xD9, 0x46, 0x8F),
        Rgb::new(0xC9, 0x8A, 0x00),
    ],
    desktop_track: Rgb::new(0x6B, 0x72, 0x80),
};

const MIDNIGHT: ThemeColors = ThemeColors {
    background: Rgb::new(0x0B, 0x10, 0x20),
    panels: Rgb::new(0x11, 0x18, 0x31),
    cards: Rgb::new(0x0E, 0x14, 0x28),
    controls: Rgb::new(0x1A, 0x23, 0x42),
    controls_hovered: Rgb::new(0x23, 0x2E, 0x55),
    borders: Rgb::new(0x26, 0x30, 0x4F),
    text: Rgb::new(0xE6, 0xEA, 0xF5),
    text_muted: Rgb::new(0x8A, 0x94, 0xB0),
    accent: Rgb::new(0x5B, 0x8C, 0xFF),
    text_on_accent: Rgb::new(0x08, 0x0D, 0x1C),
    warning: Rgb::new(0xF0, 0xB4, 0x4C),
    error: Rgb::new(0xF2, 0x6D, 0x6D),
    channels: [
        Rgb::new(0x38, 0xBD, 0xF8),
        Rgb::new(0x34, 0xD3, 0x99),
        Rgb::new(0xA7, 0x8B, 0xFA),
        Rgb::new(0xF4, 0x72, 0xB6),
        Rgb::new(0xFB, 0xBF, 0x24),
    ],
    desktop_track: Rgb::new(0x8A, 0x94, 0xB0),
};

const FOREST: ThemeColors = ThemeColors {
    background: Rgb::new(0x0F, 0x15, 0x12),
    panels: Rgb::new(0x15, 0x1D, 0x19),
    cards: Rgb::new(0x12, 0x19, 0x15),
    controls: Rgb::new(0x1E, 0x29, 0x24),
    controls_hovered: Rgb::new(0x26, 0x34, 0x2D),
    borders: Rgb::new(0x2A, 0x38, 0x30),
    text: Rgb::new(0xE4, 0xEC, 0xE6),
    text_muted: Rgb::new(0x8D, 0xA0, 0x96),
    accent: Rgb::new(0x5F, 0xD6, 0x8A),
    text_on_accent: Rgb::new(0x0B, 0x1A, 0x10),
    warning: Rgb::new(0xE8, 0xC0, 0x5A),
    error: Rgb::new(0xEF, 0x76, 0x6B),
    channels: [
        Rgb::new(0x6C, 0xB8, 0xE8),
        Rgb::new(0x9B, 0xD6, 0x6A),
        Rgb::new(0xC2, 0xA6, 0xF0),
        Rgb::new(0xEE, 0x8F, 0xB0),
        Rgb::new(0xE8, 0xC0, 0x5A),
    ],
    desktop_track: Rgb::new(0x8D, 0xA0, 0x96),
};

const SYNTHWAVE: ThemeColors = ThemeColors {
    background: Rgb::new(0x14, 0x0F, 0x1F),
    panels: Rgb::new(0x1C, 0x15, 0x30),
    cards: Rgb::new(0x18, 0x12, 0x29),
    controls: Rgb::new(0x2A, 0x20, 0x45),
    controls_hovered: Rgb::new(0x35, 0x2A, 0x57),
    borders: Rgb::new(0x3A, 0x2E, 0x5C),
    text: Rgb::new(0xF2, 0xE9, 0xFF),
    text_muted: Rgb::new(0xA3, 0x96, 0xC2),
    accent: Rgb::new(0xFF, 0x4F, 0xD8),
    text_on_accent: Rgb::new(0x1A, 0x0B, 0x1F),
    warning: Rgb::new(0xFF, 0xC8, 0x57),
    error: Rgb::new(0xFF, 0x5C, 0x7A),
    channels: [
        Rgb::new(0x2D, 0xE2, 0xE6),
        Rgb::new(0x7C, 0xFF, 0x9B),
        Rgb::new(0xB9, 0x8C, 0xFF),
        Rgb::new(0xFF, 0x7E, 0xB6),
        Rgb::new(0xFF, 0xC8, 0x57),
    ],
    desktop_track: Rgb::new(0xA3, 0x96, 0xC2),
};

/// Light grey with blue, like classic Windows programs.
const CLASSIC: ThemeColors = ThemeColors {
    background: Rgb::new(0xF0, 0xF0, 0xF0),
    panels: Rgb::new(0xF7, 0xF7, 0xF7),
    cards: Rgb::new(0xFF, 0xFF, 0xFF),
    controls: Rgb::new(0xE1, 0xE1, 0xE1),
    controls_hovered: Rgb::new(0xE5, 0xF1, 0xFB),
    borders: Rgb::new(0xAD, 0xAD, 0xAD),
    text: Rgb::new(0x00, 0x00, 0x00),
    text_muted: Rgb::new(0x5A, 0x5A, 0x5A),
    accent: Rgb::new(0x00, 0x78, 0xD7),
    text_on_accent: Rgb::new(0xFF, 0xFF, 0xFF),
    warning: Rgb::new(0x9D, 0x5D, 0x00),
    error: Rgb::new(0xC4, 0x2B, 0x1C),
    channels: [
        Rgb::new(0x00, 0x78, 0xD7),
        Rgb::new(0x10, 0x7C, 0x10),
        Rgb::new(0x88, 0x17, 0x98),
        Rgb::new(0xC3, 0x00, 0x52),
        Rgb::new(0xCA, 0x50, 0x10),
    ],
    desktop_track: Rgb::new(0x6E, 0x6E, 0x6E),
};

const HIGH_CONTRAST: ThemeColors = ThemeColors {
    background: Rgb::new(0x00, 0x00, 0x00),
    panels: Rgb::new(0x00, 0x00, 0x00),
    cards: Rgb::new(0x00, 0x00, 0x00),
    controls: Rgb::new(0x1A, 0x1A, 0x1A),
    controls_hovered: Rgb::new(0x33, 0x33, 0x33),
    borders: Rgb::new(0xFF, 0xFF, 0xFF),
    text: Rgb::new(0xFF, 0xFF, 0xFF),
    text_muted: Rgb::new(0xE0, 0xE0, 0xE0),
    accent: Rgb::new(0xFF, 0xD4, 0x00),
    text_on_accent: Rgb::new(0x00, 0x00, 0x00),
    warning: Rgb::new(0xFF, 0xD4, 0x00),
    error: Rgb::new(0xFF, 0x60, 0x60),
    channels: [
        Rgb::new(0x00, 0xE5, 0xFF),
        Rgb::new(0x00, 0xFF, 0x7F),
        Rgb::new(0xD0, 0x9C, 0xFF),
        Rgb::new(0xFF, 0x7F, 0xD0),
        Rgb::new(0xFF, 0xD4, 0x00),
    ],
    desktop_track: Rgb::new(0xC0, 0xC0, 0xC0),
};

/// The colors in use.
static CURRENT: RwLock<ThemeColors> = RwLock::new(ThemeColors::SONORA);

/// The colors in use.
pub fn colors() -> ThemeColors {
    *CURRENT.read().unwrap()
}

/// The preset `colors` are exactly, if any.
pub fn preset_name(colors: &ThemeColors) -> Option<&'static str> {
    PRESETS.iter().find(|(_, preset)| preset == colors).map(|(name, _)| *name)
}

pub fn color(rgb: Rgb) -> Color32 {
    let [r, g, b] = rgb.0;
    Color32::from_rgb(r, g, b)
}

pub fn rgb(color: Color32) -> Rgb {
    Rgb::new(color.r(), color.g(), color.b())
}

/// Window background.
pub fn bg() -> Color32 {
    color(colors().background)
}
/// Panels and windows.
pub fn surface() -> Color32 {
    color(colors().panels)
}
/// Cards: the boxes things are grouped in.
pub fn card_bg() -> Color32 {
    color(colors().cards)
}
/// Controls and raised parts of cards.
pub fn raised() -> Color32 {
    color(colors().controls)
}
/// Hovered controls.
pub fn raised_hover() -> Color32 {
    color(colors().controls_hovered)
}
pub fn border() -> Color32 {
    color(colors().borders)
}
pub fn text() -> Color32 {
    color(colors().text)
}
pub fn text_muted() -> Color32 {
    color(colors().text_muted)
}
/// Selected tabs, primary buttons, focus.
pub fn accent() -> Color32 {
    color(colors().accent)
}
/// Text and icons drawn on the accent.
pub fn on_accent() -> Color32 {
    color(colors().text_on_accent)
}
pub fn danger() -> Color32 {
    color(colors().error)
}
pub fn warning() -> Color32 {
    color(colors().warning)
}
/// The clip editor's desktop audio track.
pub fn desktop_track() -> Color32 {
    color(colors().desktop_track)
}

/// The color of channel `index` (in [`crate::FIXED_BUSES`] order).
pub fn channel_color(index: usize) -> Color32 {
    let channels = colors().channels;
    color(channels[index % channels.len()])
}

/// Black or white, whichever reads better on `fill`.
pub fn text_on(fill: Color32) -> Color32 {
    if is_light(fill) {
        Color32::from_rgb(0x14, 0x14, 0x18)
    } else {
        Color32::WHITE
    }
}

fn is_light(color: Color32) -> bool {
    // Rec. 709 luma of the sRGB values: good enough to pick a text color.
    0.2126 * f32::from(color.r()) + 0.7152 * f32::from(color.g()) + 0.0722 * f32::from(color.b()) > 140.0
}

/// Starts with your saved colors. The first time, settings from before
/// colors could be chosen bring their custom channel colors along, and the
/// result is saved (those older colors are no longer written).
pub fn load(ctx: &egui::Context) {
    let presets = common::load_presets();
    let colors = match presets.colors {
        Some(colors) => colors,
        None => {
            let mut colors = ThemeColors::SONORA;
            for (channel, bus) in colors.channels.iter_mut().zip(crate::FIXED_BUSES) {
                if let Some(old) = presets.mixer.buses.get(*bus).and_then(|b| Rgb::from_hex(b.color.as_deref()?)) {
                    *channel = old;
                }
            }
            if colors != ThemeColors::SONORA {
                let _ = common::update_presets(|presets| presets.colors = Some(colors));
            }
            colors
        }
    };
    set(ctx, colors);
}

/// Uses `colors` from now on, everywhere.
pub fn set(ctx: &egui::Context, colors: ThemeColors) {
    *CURRENT.write().unwrap() = colors;
    apply(ctx);
}

/// Sets egui's style from the colors in use.
pub fn apply(ctx: &egui::Context) {
    // Light or dark follows your background, not Windows' app mode.
    let theme = if is_light(bg()) { egui::Theme::Light } else { egui::Theme::Dark };
    ctx.set_theme(theme);
    let mut style = (*ctx.style_of(theme)).clone();
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

    let (surface, raised, raised_hover, border, text, text_muted, accent) =
        (surface(), raised(), raised_hover(), border(), text(), text_muted(), accent());
    let v = &mut style.visuals;
    *v = if theme == egui::Theme::Light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = surface;
    v.window_fill = surface;
    v.extreme_bg_color = bg();
    v.faint_bg_color = raised;
    v.code_bg_color = raised;
    v.window_stroke = Stroke::new(1.0, border);
    v.window_corner_radius = CornerRadius::same(RADIUS);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    v.selection.bg_fill = accent.linear_multiply(0.85);
    v.selection.stroke = Stroke::new(1.0, on_accent());
    v.hyperlink_color = accent;
    v.warn_fg_color = warning();
    v.error_fg_color = danger();
    v.override_text_color = None;
    // Sliders fill up to their handle, so a value reads at a glance.
    v.slider_trailing_fill = true;

    let radius = CornerRadius::same(RADIUS - 2);
    let widgets = &mut v.widgets;
    widgets.noninteractive.bg_fill = surface;
    widgets.noninteractive.weak_bg_fill = surface;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, text_muted);
    widgets.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut widgets.inactive, raised),
        (&mut widgets.hovered, raised_hover),
        (&mut widgets.active, raised_hover),
        (&mut widgets.open, raised_hover),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.bg_stroke = Stroke::new(1.0, border);
        state.fg_stroke = Stroke::new(1.0, text);
        state.expansion = 0.0;
    }
    widgets.hovered.bg_stroke = Stroke::new(1.0, text_muted);
    widgets.active.bg_stroke = Stroke::new(1.0, accent);
    ctx.set_style_of(theme, style);
}

/// A card: the frame every grouped part of a screen sits in.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(card_bg())
        .stroke(Stroke::new(1.0, border()))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(12))
}

/// A small uppercase heading over a section of a panel.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title.to_uppercase()).small().strong().color(text_muted()));
}

/// A filled accent button, for a screen's main action.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(on_accent()).strong()).fill(accent())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_distinct_names_and_colors() {
        for (i, (name, colors)) in PRESETS.iter().enumerate() {
            for (other_name, other) in &PRESETS[i + 1..] {
                assert_ne!(name, other_name);
                assert_ne!(colors, other, "{name} and {other_name} are the same colors");
            }
        }
        assert_eq!(preset_name(&ThemeColors::SONORA), Some("Sonora"));
    }

    #[test]
    fn text_on_picks_the_readable_color() {
        assert_eq!(text_on(Color32::WHITE), Color32::from_rgb(0x14, 0x14, 0x18));
        assert_eq!(text_on(Color32::from_rgb(0x12, 0x14, 0x19)), Color32::WHITE);
    }
}
