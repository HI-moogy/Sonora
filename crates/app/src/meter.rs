//! Level meters that move like hardware ones: rise instantly, fall
//! smoothly, and hold the latest peak for a moment.

use std::time::{Duration, Instant};

use eframe::egui;

/// How fast a meter falls after a peak.
const FALL_DB_PER_SECOND: f32 = 30.0;
/// How long the peak line stays put before it falls too.
const PEAK_HOLD: Duration = Duration::from_millis(1000);
/// Below this a meter counts as empty and stops asking for repaints.
const SILENT: f32 = 0.001;
const SIZE: egui::Vec2 = egui::vec2(90.0, 6.0);
/// Repaint rate while a meter is still moving.
const MOVING_REPAINT: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, Debug)]
struct Ballistics {
    shown: f32,
    hold: f32,
    held_since: Instant,
    updated: Instant,
}

impl Ballistics {
    fn new(now: Instant) -> Self {
        Self { shown: 0.0, hold: 0.0, held_since: now, updated: now }
    }

    /// Takes the peak measured since the last update.
    fn update(&mut self, peak: f32, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f32();
        let fall = 10f32.powf(-FALL_DB_PER_SECOND * elapsed / 20.0);
        let peak = peak.clamp(0.0, 1.0);
        self.shown = peak.max(self.shown * fall);
        if peak >= self.hold {
            self.hold = peak;
            self.held_since = now;
        } else if now.saturating_duration_since(self.held_since) > PEAK_HOLD {
            self.hold = (self.hold * fall).max(self.shown);
        }
        self.updated = now;
    }

    fn is_moving(&self) -> bool {
        self.shown > SILENT || self.hold > SILENT
    }
}

/// What a meter shows this frame for `peak` (the peak since the previous
/// frame): (level, held peak), both 0..1. `key` identifies the meter
/// across frames.
pub fn meter_levels(ui: &egui::Ui, key: impl std::hash::Hash + std::fmt::Debug, peak: f32) -> (f32, Option<f32>) {
    let id = egui::Id::unique(("level-meter", key));
    let now = Instant::now();
    let mut meter = ui.data_mut(|data| *data.get_temp_mut_or_insert_with(id, || Ballistics::new(now)));
    meter.update(peak, now);
    ui.data_mut(|data| data.insert_temp(id, meter));
    if meter.is_moving() {
        ui.ctx().request_repaint_after(MOVING_REPAINT);
    }
    (meter.shown, (meter.hold > SILENT).then_some(meter.hold))
}

/// Draws a horizontal meter for `peak` in `color`; see [`meter_levels`].
pub fn level_meter(ui: &mut egui::Ui, key: impl std::hash::Hash + std::fmt::Debug, peak: f32, color: egui::Color32) {
    let (shown, hold) = meter_levels(ui, key, peak);
    let (rect, _) = ui.allocate_exact_size(SIZE, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, crate::theme::bg());
    let x_at = |level: f32| rect.left() + rect.width() * level;
    let fill = egui::Rect::from_min_max(rect.min, egui::pos2(x_at(shown), rect.bottom()));
    painter.rect_filled(fill, 3.0, color);
    if let Some(hold) = hold {
        let x = x_at(hold).min(rect.right() - 1.0);
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, crate::theme::text()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_at_once_and_falls_at_the_set_rate() {
        let start = Instant::now();
        let mut meter = Ballistics::new(start);
        meter.update(0.8, start);
        assert_eq!(meter.shown, 0.8);
        // A silent second later: 30 dB down, not straight to zero.
        meter.update(0.0, start + Duration::from_secs(1));
        let expected = 0.8 * 10f32.powf(-30.0 / 20.0);
        assert!((meter.shown - expected).abs() < 1e-4, "{}", meter.shown);
    }

    #[test]
    fn a_silent_read_between_peaks_doesnt_drop_the_meter() {
        let start = Instant::now();
        let mut meter = Ballistics::new(start);
        meter.update(0.5, start);
        // One frame later it has fallen 1 dB, not to zero.
        meter.update(0.0, start + Duration::from_millis(33));
        assert!(meter.shown > 0.44, "{}", meter.shown);
    }

    #[test]
    fn peak_line_holds_then_falls() {
        let start = Instant::now();
        let mut meter = Ballistics::new(start);
        meter.update(0.9, start);
        meter.update(0.1, start + Duration::from_millis(500));
        assert_eq!(meter.hold, 0.9, "held");
        meter.update(0.1, start + Duration::from_millis(1500));
        assert!(meter.hold < 0.9 && meter.hold >= meter.shown, "{} vs {}", meter.hold, meter.shown);
        meter.update(0.95, start + Duration::from_millis(1600));
        assert_eq!(meter.hold, 0.95, "a new peak resets the hold");
    }
}
