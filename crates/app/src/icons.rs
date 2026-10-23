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
        Icon::ToStart => {
            let bar = Rect::from_center_size(center + vec2(-s * 0.38, 0.0), vec2(s * 0.16, s * 0.8));
            painter.rect_filled(bar, CornerRadius::same(1), color);
            painter.add(triangle(center + vec2(s * 0.08, 0.0), s * 0.8, false, color));
        }
    }
}

/// A rounded button showing `icon`. `primary` fills it with the accent
/// color (the main action, like play).
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, primary: bool, enabled: bool) -> egui::Response {
    let size = if primary { vec2(46.0, 38.0) } else { vec2(38.0, 32.0) };
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect.expand(1.0));
        let hovered = enabled && response.hovered();
        let (fill, stroke, color) = match (primary, enabled) {
            (true, true) => (if hovered { theme::ACCENT.linear_multiply(1.1) } else { theme::ACCENT }, Stroke::NONE, Color32::from_rgb(0x1A, 0x10, 0x0C)),
            (true, false) => (theme::ACCENT.linear_multiply(0.35), Stroke::NONE, Color32::from_rgb(0x1A, 0x10, 0x0C)),
            (false, true) => (if hovered { theme::RAISED_HOVER } else { theme::RAISED }, Stroke::new(1.0, theme::BORDER), theme::TEXT),
            (false, false) => (theme::RAISED.linear_multiply(0.5), Stroke::new(1.0, theme::BORDER), theme::TEXT_MUTED),
        };
        painter.rect(rect, CornerRadius::same(if primary { 19 } else { 8 }), fill, stroke, StrokeKind::Inside);
        let pressed = response.is_pointer_button_down_on();
        let center = rect.center() + if pressed { vec2(0.0, 1.0) } else { vec2(0.0, 0.0) };
        paint(&painter, icon, center, if primary { 15.0 } else { 12.0 }, color);
    }
    response
}
