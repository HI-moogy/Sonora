//! The getting-started guide: a short tour shown on first launch (and from
//! the menu), dimming the window except the part each step is about.
//!
//! Screens say where their parts are with [`target`]; the guide reads the
//! rects from the frame before, so a step's spotlight follows the layout.

use eframe::egui;

use crate::theme;

/// Where the app should be while a step shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The Mixer tab, with this channel's settings open (or none).
    Mixer(Option<&'static str>),
    Clips,
}

/// Which systems a step is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum On {
    All,
    Windows,
    Linux,
}

impl On {
    fn here(self) -> bool {
        match self {
            On::All => true,
            On::Windows => cfg!(windows),
            On::Linux => cfg!(target_os = "linux"),
        }
    }
}

struct Step {
    on: On,
    title: &'static str,
    /// `{hotkey}` is replaced with the clip hotkey.
    body: &'static str,
    /// The part to spotlight, as named with [`target`].
    target: Option<&'static str>,
    view: View,
}

const STEPS: &[Step] = &[
    Step {
        on: On::All,
        title: "Welcome to Sonora",
        body: "Sonora splits what you hear into channels, each with its own volume, EQ and output, cleans up \
               your mic, and keeps the last moments of your game ready to save as a clip.\n\n\
               This tour shows where everything is. It takes about a minute.",
        target: None,
        view: View::Mixer(None),
    },
    Step {
        on: On::All,
        title: "Your channels",
        body: "Game, Chat, Aux and Media are what you hear. Mic is your voice.\n\n\
               Drag a fader to change a channel's volume (double-click it for 100%), and Mute silences it. \
               The meter shows what's playing.",
        target: Some("strips"),
        view: View::Mixer(None),
    },
    Step {
        on: On::All,
        title: "Put apps in a channel",
        body: "Click a channel to open its settings here.\n\n\
               Under Add an app, click Add next to a game, Discord, Spotify or anything else playing sound. \
               Plays on picks your headset, speakers or TV, and EQ has presets for games, music and voices.",
        target: Some("channel-panel"),
        view: View::Mixer(Some("Game")),
    },
    Step {
        on: On::Windows,
        title: "Your mic",
        body: "Pick your microphone here, turn on Noise removal, and use Listen to my mic to hear how you sound.\n\n\
               For Discord and games to get the cleaned-up voice, install the free VB-Audio Virtual Cable, \
               set Send my voice to \"CABLE Input\", and choose \"CABLE Output\" as your mic in Discord.",
        target: Some("channel-panel"),
        view: View::Mixer(Some("Mic")),
    },
    Step {
        on: On::Linux,
        title: "Your mic",
        body: "Pick your microphone here, turn on Noise removal, and use Listen to my mic to hear how you sound.\n\n\
               For Discord and games to get the cleaned-up voice, set Send my voice to \"Sonora Virtual \
               Microphone\", and choose it as your mic in Discord too. Nothing to install.",
        target: Some("channel-panel"),
        view: View::Mixer(Some("Mic")),
    },
    Step {
        on: On::All,
        title: "Master volume",
        body: "Master turns everything you hear up or down at once. It doesn't change the voice you send.",
        target: Some("master"),
        view: View::Mixer(None),
    },
    Step {
        on: On::Windows,
        title: "Clips",
        body: "Sonora keeps the last moments of your screen ready. Press {hotkey} right after something \
               happens to save it as a clip; a sound plays when it's saved.\n\n\
               Your clips show up in this tab. Click one to watch it, change each audio track's volume, \
               and save a copy.",
        target: Some("clips-tab"),
        view: View::Clips,
    },
    Step {
        on: On::Windows,
        title: "Clip settings",
        body: "The gear has what to record (the whole screen, or only games), how many seconds to keep, \
               the hotkey, and where clips are saved.",
        target: Some("clip-settings"),
        view: View::Clips,
    },
    Step {
        on: On::Windows,
        title: "It keeps running",
        body: "Closing the window keeps Sonora in the tray by the clock, so your audio keeps flowing and clips \
               keep recording. Right-click the tray icon for Start with Windows and Quit.\n\n\
               Menu has Colors, to make Sonora look the way you like, and this guide if you want it again.",
        target: Some("menu"),
        view: View::Mixer(None),
    },
    Step {
        on: On::Linux,
        title: "It keeps running",
        body: "Closing the window keeps Sonora in the tray, so your audio keeps flowing. Right-click the tray \
               icon for Start at login and Quit. (No tray on your desktop? Then closing the window quits.)\n\n\
               Menu has Colors, to make Sonora look the way you like, and this guide if you want it again.",
        target: Some("menu"),
        view: View::Mixer(None),
    },
];

/// The steps for this system, in order.
fn steps() -> impl Iterator<Item = &'static Step> {
    STEPS.iter().filter(|step| step.on.here())
}

fn step_count() -> usize {
    steps().count()
}

/// The card's width.
const CARD_WIDTH: f32 = 400.0;
/// Room between the spotlight and the card.
const GAP: f32 = 18.0;

/// Says where a part of the screen is this frame, for the guide to point at.
pub fn target(ctx: &egui::Context, name: &'static str, rect: egui::Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(target_id(name), (rect, pass)));
}

fn target_id(name: &str) -> egui::Id {
    egui::Id::unique(("guide-target", name))
}

/// Where `name` was last frame, if it was drawn then.
fn target_rect(ctx: &egui::Context, name: &str) -> Option<egui::Rect> {
    let (rect, pass) = ctx.data(|d| d.get_temp::<(egui::Rect, u64)>(target_id(name)))?;
    (ctx.cumulative_pass_nr() <= pass + 2).then_some(rect)
}

/// What happened to the guide this frame.
pub enum Outcome {
    Showing,
    /// Finished or skipped: it's been seen.
    Done,
}

#[derive(Default)]
pub struct Guide {
    step: Option<usize>,
}

impl Guide {
    pub fn start(&mut self) {
        self.step = Some(0);
    }

    /// Where the app should be for the current step.
    pub fn view(&self) -> Option<View> {
        self.step.and_then(|step| steps().nth(step)).map(|step| step.view)
    }

    /// Draws the spotlight and the step's card over everything.
    pub fn show(&mut self, ctx: &egui::Context, hotkey: &str) -> Option<Outcome> {
        let index = self.step?;
        let step = steps().nth(index)?;
        let count = step_count();
        let screen = ctx.content_rect();
        let spotlight = step.target.and_then(|name| target_rect(ctx, name)).map(|rect| rect.expand(6.0).intersect(screen));
        if step.target.is_some() && spotlight.is_none() {
            // The part is drawn next frame (the view just changed), or isn't
            // on screen (the clip editor covers the clip list).
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Dim everything but the spotlight, and keep clicks off the app.
        egui::Area::new(egui::Id::unique("guide-dim"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.allocate_rect(screen, egui::Sense::click_and_drag());
                let painter = ui.painter();
                let dim = egui::Color32::from_black_alpha(165);
                match spotlight {
                    Some(hole) => {
                        for rect in around(screen, hole) {
                            painter.rect_filled(rect, 0.0, dim);
                        }
                        painter.rect_stroke(hole, 8.0, egui::Stroke::new(2.0, theme::accent()), egui::StrokeKind::Outside);
                    }
                    None => {
                        painter.rect_filled(screen, 0.0, dim);
                    }
                }
            });

        let card_id = egui::Id::unique("guide-card");
        let height = ctx.memory(|m| m.area_rect(card_id)).map_or(240.0, |rect| rect.height());
        let size = egui::vec2(CARD_WIDTH, height);
        let pos = match spotlight {
            Some(hole) => card_position(screen, hole, size),
            None => screen.center() - size / 2.0,
        };
        let mut next = None;
        egui::Area::new(card_id).order(egui::Order::Tooltip).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::surface())
                .stroke(egui::Stroke::new(1.0, theme::border()))
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(18))
                .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: egui::Color32::from_black_alpha(120) })
                .show(ui, |ui| {
                    ui.set_width(CARD_WIDTH - 36.0);
                    ui.label(
                        egui::RichText::new(format!("{} of {count}", index + 1)).small().color(theme::text_muted()),
                    );
                    ui.label(egui::RichText::new(step.title).size(18.0).strong().color(theme::text()));
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(step.body.replace("{hotkey}", hotkey)).color(theme::text()));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if index + 1 < count
                            && ui.add(egui::Button::new(egui::RichText::new("Skip tour").color(theme::text_muted())).frame(false)).clicked()
                        {
                            next = Some(None);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let last = index + 1 == count;
                            if ui.add(theme::primary_button(if last { "Start using Sonora" } else { "Next" })).clicked() {
                                next = Some((!last).then_some(index + 1));
                            }
                            if index > 0 && ui.button("Back").clicked() {
                                next = Some(Some(index - 1));
                            }
                        });
                    });
                });
        });

        let keys = ctx.input(|i| {
            (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::ArrowRight), i.key_pressed(egui::Key::ArrowLeft))
        });
        match keys {
            (true, ..) => next = Some(None),
            (_, true, _) if next.is_none() => next = Some((index + 1 < count).then_some(index + 1)),
            (_, _, true) if next.is_none() && index > 0 => next = Some(Some(index - 1)),
            _ => {}
        }
        match next {
            Some(step) => {
                self.step = step;
                ctx.request_repaint();
                Some(if step.is_some() { Outcome::Showing } else { Outcome::Done })
            }
            None => Some(Outcome::Showing),
        }
    }
}

/// The four rects of `screen` around `hole`.
fn around(screen: egui::Rect, hole: egui::Rect) -> [egui::Rect; 4] {
    [
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, hole.min.y)),
        egui::Rect::from_min_max(egui::pos2(screen.min.x, hole.max.y), screen.max),
        egui::Rect::from_min_max(egui::pos2(screen.min.x, hole.min.y), egui::pos2(hole.min.x, hole.max.y)),
        egui::Rect::from_min_max(egui::pos2(hole.max.x, hole.min.y), egui::pos2(screen.max.x, hole.max.y)),
    ]
}

/// Where the card goes: below the spotlight if it fits, else above, else
/// beside it, else in the middle of the screen; always on screen.
fn card_position(screen: egui::Rect, hole: egui::Rect, size: egui::Vec2) -> egui::Pos2 {
    let inner = screen.shrink(12.0);
    let centered_x = (hole.center().x - size.x / 2.0).clamp(inner.left(), (inner.right() - size.x).max(inner.left()));
    let centered_y = (hole.center().y - size.y / 2.0).clamp(inner.top(), (inner.bottom() - size.y).max(inner.top()));
    if hole.bottom() + GAP + size.y <= inner.bottom() {
        egui::pos2(centered_x, hole.bottom() + GAP)
    } else if hole.top() - GAP - size.y >= inner.top() {
        egui::pos2(centered_x, hole.top() - GAP - size.y)
    } else if hole.left() - GAP - size.x >= inner.left() {
        egui::pos2(hole.left() - GAP - size.x, centered_y)
    } else if hole.right() + GAP + size.x <= inner.right() {
        egui::pos2(hole.right() + GAP, centered_y)
    } else {
        screen.center() - size / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_goes_below_then_above_then_beside() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let size = egui::vec2(CARD_WIDTH, 200.0);
        let top = egui::Rect::from_min_size(egui::pos2(500.0, 20.0), egui::vec2(100.0, 40.0));
        assert_eq!(card_position(screen, top, size).y, top.bottom() + GAP);
        let bottom = egui::Rect::from_min_size(egui::pos2(500.0, 700.0), egui::vec2(100.0, 60.0));
        assert_eq!(card_position(screen, bottom, size).y, bottom.top() - GAP - size.y);
        let right_panel = egui::Rect::from_min_max(egui::pos2(760.0, 60.0), egui::pos2(1200.0, 800.0));
        assert_eq!(card_position(screen, right_panel, size).x, right_panel.left() - GAP - size.x);
    }

    #[test]
    fn the_tour_starts_with_a_welcome_and_every_step_has_words() {
        assert!(steps().next().is_some_and(|step| step.target.is_none()));
        for step in STEPS {
            assert!(!step.title.is_empty() && !step.body.is_empty());
        }
        // Clips are Windows-only, so Linux's tour never goes to them.
        if !cfg!(windows) {
            assert!(steps().all(|step| step.view != View::Clips));
        }
    }
}
