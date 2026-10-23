//! A channel strip: the card for one channel in the Mixer, with its name,
//! a fader with the level meter beside it, the volume, and a mute button.
//! Clicking the card opens the channel's settings.

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind};

use crate::meter::meter_levels;
use crate::theme;

const SIZE: egui::Vec2 = vec2(116.0, 340.0);
const FADER_HEIGHT: f32 = 200.0;
/// Highest volume a fader reaches (2 = +6 dB).
const MAX_GAIN: f32 = 2.0;

/// Fader position (0 bottom, 1 top) for a volume. The curve puts 100%
/// about two-thirds up, leaving most of the travel for turning down,
/// which is where fine control matters.
fn position(gain: f32) -> f32 {
    (gain / MAX_GAIN).clamp(0.0, 1.0).sqrt()
}

fn gain_at(position: f32) -> f32 {
    position.clamp(0.0, 1.0).powi(2) * MAX_GAIN
}

/// What the user did to a strip this frame.
#[derive(Default)]
pub struct StripResponse {
    pub selected: bool,
    pub gain: Option<f32>,
    pub toggle_mute: bool,
}

pub struct Strip<'a> {
    pub name: &'a str,
    pub color: Color32,
    pub gain: f32,
    pub muted: bool,
    /// Peak since the previous frame.
    pub peak: f32,
    /// What to show under the name ("2 apps", "Not sent"...).
    pub detail: String,
    pub selected: bool,
}

impl Strip<'_> {
    pub fn show(self, ui: &mut egui::Ui) -> StripResponse {
        let mut out = StripResponse::default();
        let (card, card_response) = ui.allocate_exact_size(SIZE, Sense::click());
        if card_response.clicked() {
            out.selected = true;
        }
        let painter = ui.painter_at(card.expand(2.0));
        let hovered = card_response.hovered();
        let fill = if self.selected {
            self.color.linear_multiply(0.10)
        } else if hovered {
            theme::raised().linear_multiply(0.8)
        } else {
            theme::raised().linear_multiply(0.55)
        };
        let stroke = if self.selected { Stroke::new(1.5, self.color) } else { Stroke::new(1.0, theme::border()) };
        painter.rect(card, CornerRadius::same(theme::RADIUS + 2), fill, stroke, StrokeKind::Inside);
        // A band of the channel's color across the top.
        let band = Rect::from_min_size(card.min + vec2(14.0, 0.0), vec2(card.width() - 28.0, 3.0));
        painter.rect_filled(band, CornerRadius::same(2), self.color);

        painter.text(pos2(card.center().x, card.top() + 24.0), Align2::CENTER_CENTER, self.name, FontId::proportional(15.0), theme::text());
        painter.text(
            pos2(card.center().x, card.top() + 43.0),
            Align2::CENTER_CENTER,
            &self.detail,
            FontId::proportional(11.5),
            theme::text_muted(),
        );

        // Fader track, with the meter just right of it.
        let track = Rect::from_center_size(pos2(card.center().x - 8.0, card.top() + 66.0 + FADER_HEIGHT / 2.0), vec2(8.0, FADER_HEIGHT));
        let meter = Rect::from_min_size(pos2(track.right() + 12.0, track.top()), vec2(6.0, FADER_HEIGHT));
        let y_at = |p: f32| track.bottom() - p * track.height();

        let fader_id = ui.scope_id().with(("fader", self.name));
        let hit = track.expand2(vec2(18.0, 10.0));
        let fader = ui.interact(hit, fader_id, Sense::click_and_drag());
        let mut p = position(self.gain);
        if fader.double_clicked() {
            p = position(1.0);
            out.gain = Some(1.0);
        } else if let Some(pointer) = fader.interact_pointer_pos().filter(|_| fader.dragged() || fader.clicked()) {
            p = ((track.bottom() - pointer.y) / track.height()).clamp(0.0, 1.0);
            out.gain = Some(gain_at(p));
        }
        if fader.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                p = (p + scroll * 0.0015).clamp(0.0, 1.0);
                out.gain = Some(gain_at(p));
            }
        }
        let fader = fader.on_hover_text("Drag or scroll to change. Double-click for 100%.");

        painter.rect_filled(track, CornerRadius::same(4), theme::bg());
        let fill_color = if self.muted { theme::text_muted().linear_multiply(0.5) } else { self.color };
        let filled = Rect::from_min_max(pos2(track.left(), y_at(p)), track.max);
        painter.rect_filled(filled, CornerRadius::same(4), fill_color.linear_multiply(0.85));
        // 100% mark.
        let unity = y_at(position(1.0));
        painter.line_segment(
            [pos2(track.left() - 7.0, unity), pos2(track.left() - 3.0, unity)],
            Stroke::new(1.5, theme::text_muted()),
        );
        let thumb = Rect::from_center_size(pos2(track.center().x, y_at(p)), vec2(30.0, 12.0));
        let thumb_stroke = if fader.hovered() || fader.dragged() { self.color } else { theme::border() };
        painter.rect(thumb, CornerRadius::same(4), theme::text(), Stroke::new(1.5, thumb_stroke), StrokeKind::Inside);

        let (level, hold) = meter_levels(ui, ("strip", self.name), self.peak);
        painter.rect_filled(meter, CornerRadius::same(3), theme::bg());
        if level > 0.0 {
            let top = meter.bottom() - level.sqrt() * meter.height();
            painter.rect_filled(Rect::from_min_max(pos2(meter.left(), top), meter.max), CornerRadius::same(3), self.color);
        }
        if let Some(hold) = hold {
            let y = meter.bottom() - hold.sqrt() * meter.height();
            painter.line_segment([pos2(meter.left(), y), pos2(meter.right(), y)], Stroke::new(2.0, theme::text()));
        }

        let percent = format!("{:.0}%", gain_at(p) * 100.0);
        painter.text(
            pos2(card.center().x, track.bottom() + 18.0),
            Align2::CENTER_CENTER,
            percent,
            FontId::proportional(13.0),
            if self.muted { theme::text_muted() } else { theme::text() },
        );

        // Mute button.
        let mute = Rect::from_center_size(pos2(card.center().x, card.bottom() - 26.0), vec2(card.width() - 36.0, 26.0));
        let mute_response = ui.interact(mute, ui.scope_id().with(("mute", self.name)), Sense::click());
        if mute_response.clicked() {
            out.toggle_mute = true;
        }
        let (mute_fill, mute_text) = if self.muted {
            (theme::danger(), theme::text_on(theme::danger()))
        } else if mute_response.hovered() {
            (theme::raised_hover(), theme::text())
        } else {
            (theme::raised(), theme::text_muted())
        };
        painter.rect(mute, CornerRadius::same(6), mute_fill, Stroke::new(1.0, theme::border()), StrokeKind::Inside);
        painter.text(
            mute.center(),
            Align2::CENTER_CENTER,
            if self.muted { "Muted" } else { "Mute" },
            FontId::proportional(12.5),
            mute_text,
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_curve_round_trips_and_puts_100_percent_two_thirds_up() {
        for gain in [0.0, 0.25, 1.0, 1.5, 2.0] {
            assert!((gain_at(position(gain)) - gain).abs() < 1e-5, "{gain}");
        }
        let unity = position(1.0);
        assert!((0.65..0.75).contains(&unity), "{unity}");
    }
}
