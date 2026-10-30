//! A channel's EQ, after SteelSeries Sonar's: its curve, with a dot for
//! each band. Drag a dot to move the band (left and right: where; up and
//! down: how much), scroll on it to make it wider or narrower, double-click
//! it to flatten it, and right-click it for what it does. The picked band's
//! values can be typed in below the curve.

use audio_engine::{
    response_db, BusInfo, EqBand, EqKind, Mixer, DEFAULT_EQ_BANDS, EQ_BANDS, EQ_FREQ_RANGE, EQ_GAIN_RANGE_DB, EQ_Q_RANGE,
};
use eframe::egui;

use crate::theme;

const SAMPLE_RATE: f32 = 48_000.0;
/// The curve shows this far above and below 0 dB; cuts go off the bottom.
const SHOWN_DB: f32 = 15.0;
/// How close (px) the pointer must be to a dot to grab it.
const GRAB: f32 = 14.0;
const DOT: f32 = 6.0;
const KINDS: [(EqKind, &str); 5] = [
    (EqKind::Peak, "Peak"),
    (EqKind::LowShelf, "Low shelf"),
    (EqKind::HighShelf, "High shelf"),
    (EqKind::LowCut, "Low cut"),
    (EqKind::HighCut, "High cut"),
];
/// The frequencies named across the top of the big curve, with where they end.
const REGIONS: [(&str, f32, f32); 6] = [
    ("Sub bass", 20.0, 60.0),
    ("Bass", 60.0, 250.0),
    ("Low mids", 250.0, 500.0),
    ("Mid range", 500.0, 2000.0),
    ("Upper mids", 2000.0, 6000.0),
    ("Highs", 6000.0, 20_000.0),
];

/// What you're doing in a channel's EQ, from frame to frame.
#[derive(Default)]
pub struct EqEditor {
    /// The band picked: its values show below the curve.
    selected: usize,
    /// The band being dragged.
    dragging: Option<usize>,
    /// Whether the big EQ window is open.
    pub big: bool,
}

/// Where the curve is drawn, and how frequencies and gains map onto it.
struct Plot {
    rect: egui::Rect,
}

impl Plot {
    fn x(&self, freq: f32) -> f32 {
        let span = (EQ_FREQ_RANGE.1 / EQ_FREQ_RANGE.0).ln();
        self.rect.left() + (freq / EQ_FREQ_RANGE.0).ln() / span * self.rect.width()
    }

    fn freq(&self, x: f32) -> f32 {
        let span = (EQ_FREQ_RANGE.1 / EQ_FREQ_RANGE.0).ln();
        EQ_FREQ_RANGE.0 * ((x - self.rect.left()) / self.rect.width() * span).exp()
    }

    fn y(&self, db: f32) -> f32 {
        self.rect.center().y - db / SHOWN_DB * self.rect.height() / 2.0
    }

    fn db(&self, y: f32) -> f32 {
        (self.rect.center().y - y) / (self.rect.height() / 2.0) * SHOWN_DB
    }

    /// Where a band's dot is: cuts sit on the 0 dB line.
    fn dot(&self, band: &EqBand) -> egui::Pos2 {
        let gain = if has_gain(band.kind) { band.gain_db } else { 0.0 };
        egui::pos2(self.x(band.freq), self.y(gain))
    }
}

fn has_gain(kind: EqKind) -> bool {
    matches!(kind, EqKind::Peak | EqKind::LowShelf | EqKind::HighShelf)
}

impl EqEditor {
    /// `bus`'s curve, `height` tall (with the frequency regions named when
    /// `regions`), and the picked band's values. Changes go straight to its
    /// EQ in the mixer.
    pub fn ui(&mut self, ui: &mut egui::Ui, mixer: &Mixer, bus: &BusInfo, height: f32, regions: bool) {
        self.selected = self.selected.min(EQ_BANDS - 1);
        let mut changed: Option<(usize, EqBand)> = None;
        self.curve(ui, &bus.eq, bus.eq_enabled, height, regions, &mut changed);
        self.band_values(ui, &bus.eq, &mut changed);
        if let Some((index, band)) = changed {
            mixer.set_bus_eq_band(&bus.name, index, band);
        }
    }

    fn curve(
        &mut self,
        ui: &mut egui::Ui,
        bands: &[EqBand; EQ_BANDS],
        enabled: bool,
        height: f32,
        regions: bool,
        changed: &mut Option<(usize, EqBand)>,
    ) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
        let top = if regions { 22.0 } else { 6.0 };
        let plot = Plot { rect: egui::Rect::from_min_max(rect.min + egui::vec2(36.0, top), rect.max - egui::vec2(8.0, 20.0)) };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, theme::raised());

        // The grid, labeled.
        let grid = egui::Stroke::new(1.0, theme::border());
        let small = egui::FontId::proportional(10.0);
        for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            let y = plot.y(db);
            painter.line_segment([egui::pos2(plot.rect.left(), y), egui::pos2(plot.rect.right(), y)], grid);
            let label = if db > 0.0 { format!("+{db:.0}") } else { format!("{db:.0}") };
            painter.text(egui::pos2(plot.rect.left() - 6.0, y), egui::Align2::RIGHT_CENTER, label, small.clone(), theme::text_muted());
        }
        for freq in [20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0] {
            let x = plot.x(freq);
            painter.line_segment([egui::pos2(x, plot.rect.top()), egui::pos2(x, plot.rect.bottom())], grid);
            painter.text(egui::pos2(x, plot.rect.bottom() + 3.0), egui::Align2::CENTER_TOP, frequency_label(freq), small.clone(), theme::text_muted());
        }
        if regions {
            for (name, from, to) in REGIONS {
                let (left, right) = (plot.x(from), plot.x(to));
                let band = egui::Rect::from_min_max(egui::pos2(left + 1.0, rect.top() + 3.0), egui::pos2(right - 1.0, rect.top() + 18.0));
                painter.rect_filled(band, 3.0, theme::card_bg());
                painter.text(band.center(), egui::Align2::CENTER_CENTER, name, small.clone(), theme::text_muted());
            }
        }

        // The curve: what the bands do together.
        let points: Vec<egui::Pos2> = (0..=240)
            .map(|step| {
                let x = plot.rect.left() + plot.rect.width() * step as f32 / 240.0;
                let db = if enabled { response_db(bands, plot.freq(x), SAMPLE_RATE) } else { 0.0 };
                egui::pos2(x, plot.y(db).clamp(plot.rect.top(), plot.rect.bottom()))
            })
            .collect();
        let curve = if enabled { theme::text() } else { theme::text_muted() };
        painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, curve)));

        // Grabbing, moving and widening dots.
        let nearest = |pos: egui::Pos2| {
            (0..EQ_BANDS)
                .map(|index| (index, plot.dot(&bands[index]).distance(pos)))
                .filter(|&(_, distance)| distance <= GRAB)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index)
        };
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        if response.drag_started() {
            // The dot under where the press began: by the time a drag
            // counts as one, the pointer has moved off it.
            let pressed = ui.input(|input| input.pointer.press_origin()).or(pointer);
            self.dragging = pressed.and_then(nearest);
            if let Some(index) = self.dragging {
                self.selected = index;
            }
        }
        if let (Some(index), Some(pos)) = (self.dragging, pointer) {
            if response.dragged() {
                let mut band = bands[index];
                band.freq = plot.freq(pos.x);
                if has_gain(band.kind) {
                    band.gain_db = plot.db(pos.y).clamp(-EQ_GAIN_RANGE_DB, EQ_GAIN_RANGE_DB);
                }
                *changed = Some((index, band));
            }
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        let hovered = response.hover_pos().and_then(nearest);
        if response.clicked() || response.secondary_clicked() {
            if let Some(index) = pointer.and_then(nearest) {
                self.selected = index;
            }
        }
        if response.double_clicked() {
            if let Some(index) = pointer.and_then(nearest) {
                *changed = Some((index, EqBand { gain_db: 0.0, ..bands[index] }));
            }
        }
        if let Some(index) = hovered {
            // Scrolling over a dot widens or narrows its band, rather than
            // scrolling the panel.
            let scroll = ui.input_mut(|input| std::mem::take(&mut input.smooth_scroll_delta).y);
            if scroll != 0.0 {
                let band = bands[index];
                let q = (band.q * (scroll * 0.004).exp()).clamp(EQ_Q_RANGE.0, EQ_Q_RANGE.1);
                *changed = Some((index, EqBand { q, ..band }));
                self.selected = index;
            }
            let band = bands[index];
            response.clone().on_hover_text(format!("{}\n{}", band_summary(&band), "Drag to move · scroll to widen · right-click for more"));
        }
        response.context_menu(|ui| {
            let index = self.selected;
            let band = bands[index];
            ui.label(egui::RichText::new(format!("Band {}", index + 1)).small().color(theme::text_muted()));
            for (kind, name) in KINDS {
                if ui.selectable_label(band.kind == kind, name).clicked() {
                    *changed = Some((index, EqBand { kind, ..band }));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(if band.on { "Turn off" } else { "Turn on" }).clicked() {
                *changed = Some((index, EqBand { on: !band.on, ..band }));
                ui.close();
            }
            if ui.button("Reset this band").clicked() {
                *changed = Some((index, DEFAULT_EQ_BANDS[index]));
                ui.close();
            }
        });

        // The dots, the picked one ringed; a band that's off is hollow.
        let shown = changed.map_or(*bands, |(index, band)| {
            let mut shown = *bands;
            shown[index] = band;
            shown
        });
        for (index, band) in shown.iter().enumerate() {
            let center = plot.dot(band);
            let color = theme::eq_band_color(index);
            let radius = if index == self.selected || hovered == Some(index) { DOT + 1.5 } else { DOT };
            if band.on && enabled {
                painter.circle_filled(center, radius, color);
            } else {
                painter.circle_stroke(center, radius - 1.0, egui::Stroke::new(2.0, color));
            }
            if index == self.selected {
                painter.circle_stroke(center, radius + 2.5, egui::Stroke::new(1.5, theme::text()));
            }
        }
    }

    /// The picked band's values, to type in.
    fn band_values(&mut self, ui: &mut egui::Ui, bands: &[EqBand; EQ_BANDS], changed: &mut Option<(usize, EqBand)>) {
        let index = self.selected;
        let mut band = changed.filter(|(at, _)| *at == index).map_or(bands[index], |(_, band)| band);
        let before = band;
        ui.horizontal_wrapped(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, theme::eq_band_color(index));
            ui.label(format!("Band {}", index + 1));
            egui::ComboBox::from_id_salt("eq-band-kind")
                .width(96.0)
                .selected_text(KINDS.iter().find(|(kind, _)| *kind == band.kind).map_or("", |(_, name)| *name))
                .show_ui(ui, |ui| {
                    for (kind, name) in KINDS {
                        ui.selectable_value(&mut band.kind, kind, name);
                    }
                });
            // Moves by a percent of where it is, as the scale is logarithmic.
            let speed = band.freq * 0.01;
            ui.add(
                egui::DragValue::new(&mut band.freq)
                    .range(EQ_FREQ_RANGE.0..=EQ_FREQ_RANGE.1)
                    .speed(speed)
                    .custom_formatter(|freq, _| frequency_label(freq as f32))
                    .custom_parser(parse_frequency),
            )
            .on_hover_text("Where: drag, or type it (like 440 or 2.6k)");
            ui.add_enabled(
                has_gain(band.kind),
                egui::DragValue::new(&mut band.gain_db).range(-EQ_GAIN_RANGE_DB..=EQ_GAIN_RANGE_DB).speed(0.1).fixed_decimals(1).suffix(" dB"),
            )
            .on_hover_text("How much: up raises, down lowers");
            ui.add(egui::DragValue::new(&mut band.q).range(EQ_Q_RANGE.0..=EQ_Q_RANGE.1).speed(0.02).fixed_decimals(2).prefix("Q "))
                .on_hover_text("How wide: lower is wider, higher is narrower");
            ui.checkbox(&mut band.on, "On");
        });
        if band != before {
            *changed = Some((index, band));
        }
    }
}

/// "125 Hz", "2.6 kHz".
fn frequency_label(freq: f32) -> String {
    if freq >= 1000.0 {
        let khz = freq / 1000.0;
        if (khz - khz.round()).abs() < 0.05 { format!("{khz:.0}k") } else { format!("{khz:.1}k") }
    } else {
        format!("{freq:.0}")
    }
}

/// Reads "440", "440 Hz", "2.6k" or "2.6 kHz".
fn parse_frequency(text: &str) -> Option<f64> {
    let text = text.trim().to_lowercase();
    let text = text.trim_end_matches("hz").trim();
    match text.strip_suffix('k') {
        Some(khz) => khz.trim().parse::<f64>().ok().map(|khz| khz * 1000.0),
        None => text.parse().ok(),
    }
}

/// "Peak · 125 Hz · +3.0 dB · Q 1.41".
fn band_summary(band: &EqBand) -> String {
    let kind = KINDS.iter().find(|(kind, _)| *kind == band.kind).map_or("", |(_, name)| *name);
    let mut parts = vec![kind.to_string(), format!("{} Hz", frequency_label(band.freq))];
    if has_gain(band.kind) {
        parts.push(format!("{:+.1} dB", band.gain_db));
    }
    parts.push(format!("Q {:.2}", band.q));
    if !band.on {
        parts.push("off".into());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_read_and_show_like_people_write_them() {
        assert_eq!(frequency_label(125.0), "125");
        assert_eq!(frequency_label(2000.0), "2k");
        assert_eq!(frequency_label(2640.0), "2.6k");
        assert_eq!(parse_frequency("440"), Some(440.0));
        assert_eq!(parse_frequency("2.6k"), Some(2600.0));
        assert_eq!(parse_frequency("2.6 kHz"), Some(2600.0));
        assert_eq!(parse_frequency("80 Hz"), Some(80.0));
        assert_eq!(parse_frequency("loud"), None);
    }

    #[test]
    fn the_curve_maps_both_ways() {
        let plot = Plot { rect: egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(600.0, 200.0)) };
        assert!((plot.freq(plot.x(440.0)) - 440.0).abs() < 0.01);
        assert!((plot.db(plot.y(-6.0)) + 6.0).abs() < 0.001);
        assert_eq!(plot.x(EQ_FREQ_RANGE.0), 10.0);
        assert_eq!(plot.y(0.0), 110.0);
    }
}
