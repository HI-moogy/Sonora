//! Icon buttons drawn with shapes rather than emoji, so they look the same
//! on every PC and match Sonora's style: rounded buttons, the main one in
//! the coral accent like the app icon.

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, StrokeKind};

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Back to the start: a bar and a left-pointing triangle.
    ToStart,
    /// Skip back: two left-pointing triangles.
    Back,
    Play,
    Pause,
    /// Skip forward: two right-pointing triangles.
    Forward,
    /// To the end: a right-pointing triangle and a bar.
    ToEnd,
    /// The previous or next item: one triangle.
    Previous,
    Next,
}

/// A triangle pointing left or right, `size` tall, centered on `center`.
fn triangle(center: Pos2, size: f32, right: bool, color: Color32) -> Shape {
    let half = size / 2.0;
    let width = size * 0.85;
    let (tip, back) = if right { (center.x + width / 2.0, center.x - width / 2.0) } else { (center.x - width / 2.0, center.x + width / 2.0) };
    Shape::convex_polygon(
        vec![pos2(back, center.y - half), pos2(tip, center.y), pos2(back, center.y + half)],
        color,
        Stroke::NONE,
    )
}

fn paint(painter: &egui::Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    let s = size;
    match icon {
        Icon::Play => {
            // Nudged right: a triangle's visual center is left of its box's.
            painter.add(triangle(center + vec2(s * 0.08, 0.0), s, true, color));
        }
        Icon::Pause => {
            for dx in [-0.22, 0.22] {
                let bar = Rect::from_center_size(center + vec2(s * dx, 0.0), vec2(s * 0.24, s));
                painter.rect_filled(bar, CornerRadius::same(2), color);
            }
        }
        Icon::Back | Icon::Forward => {
            let right = icon == Icon::Forward;
            for dx in [-0.26, 0.26] {
                painter.add(triangle(center + vec2(s * dx, 0.0), s * 0.8, right, color));
            }
        }
        Icon::ToStart | Icon::ToEnd => {
            let side = if icon == Icon::ToEnd { 1.0 } else { -1.0 };
            let bar = Rect::from_center_size(center + vec2(side * s * 0.38, 0.0), vec2(s * 0.16, s * 0.8));
            painter.rect_filled(bar, CornerRadius::same(1), color);
            painter.add(triangle(center + vec2(-side * s * 0.08, 0.0), s * 0.8, icon == Icon::ToEnd, color));
        }
        Icon::Previous | Icon::Next => {
            let right = icon == Icon::Next;
            let nudge = if right { 0.08 } else { -0.08 };
            painter.add(triangle(center + vec2(s * nudge, 0.0), s, right, color));
        }
    }
}

/// Colors for a button: its fill, its outline and its icon.
fn colors(accent: bool, enabled: bool, hovered: bool) -> (Color32, Stroke, Color32) {
    let on_accent = Color32::from_rgb(0x1A, 0x10, 0x0C);
    match (accent, enabled) {
        (true, true) => (if hovered { theme::ACCENT.linear_multiply(1.1) } else { theme::ACCENT }, Stroke::NONE, on_accent),
        (true, false) => (theme::ACCENT.linear_multiply(0.35), Stroke::NONE, on_accent),
        (false, true) => (if hovered { theme::RAISED_HOVER } else { theme::RAISED }, Stroke::new(1.0, theme::BORDER), theme::TEXT),
        (false, false) => (theme::RAISED.linear_multiply(0.5), Stroke::new(1.0, theme::BORDER), theme::TEXT_MUTED),
    }
}

/// Draws a button of `size` and returns its response.
fn button(ui: &mut egui::Ui, icon: Icon, size: egui::Vec2, radius: u8, accent: bool, icon_size: f32, enabled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect.expand(1.0));
        let (fill, stroke, color) = colors(accent, enabled, enabled && response.hovered());
        painter.rect(rect, CornerRadius::same(radius), fill, stroke, StrokeKind::Inside);
        let pressed = response.is_pointer_button_down_on();
        let center = rect.center() + if pressed { vec2(0.0, 1.0) } else { vec2(0.0, 0.0) };
        paint(&painter, icon, center, icon_size, color);
    }
    response
}

/// A rounded button showing `icon`. `primary` fills it with the accent
/// color (the main action, like play).
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, primary: bool, enabled: bool) -> egui::Response {
    if primary {
        button(ui, icon, vec2(46.0, 38.0), 19, true, 15.0, enabled)
    } else {
        button(ui, icon, vec2(38.0, 32.0), 8, false, 12.0, enabled)
    }
}

/// A big square accent button, `size` wide, beside something it moves
/// through (the previous and next clip, either side of the video).
pub fn big_icon_button(ui: &mut egui::Ui, icon: Icon, size: f32, enabled: bool) -> egui::Response {
    button(ui, icon, vec2(size, size), 12, true, size * 0.36, enabled)
}
