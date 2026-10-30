//! Saying which game a clip is from, where its file and folder say nothing
//! (Radeon's "unknown" replays) or something wrong. The clip stays where
//! it is: your choice is kept with your tags (see [`common::ClipTags`]).

use std::path::{Path, PathBuf};

use eframe::egui;

use super::display_name;
use crate::theme;

/// How many games to suggest at once.
const SUGGESTIONS: usize = 8;

/// The "Which game?" window for one clip.
pub struct GameWindow {
    clip: PathBuf,
    name: String,
    /// The game it's under now.
    current: String,
    typed: String,
    /// Whether the game was set by you, so it can go back.
    chosen: bool,
    focus: bool,
}

/// What happened in the window.
pub enum GameChoice {
    /// Still open.
    Open,
    Closed,
    /// The game picked, or `None` to go back to what Sonora found.
    Set(PathBuf, Option<String>),
}

impl GameWindow {
    /// For `clip`, now under `game` (`chosen`: by you, to change from).
    pub fn open(clip: &Path, name: &str, game: &str, chosen: bool) -> Self {
        let typed = if chosen { game.to_string() } else { String::new() };
        Self { clip: clip.to_path_buf(), name: name.to_string(), current: game.to_string(), typed, chosen, focus: true }
    }

    /// `known`: games to suggest (yours and installed ones), by name.
    pub fn show(&mut self, ctx: &egui::Context, known: &[String]) -> GameChoice {
        let mut open = true;
        let mut choice = GameChoice::Open;
        egui::Window::new("Which game is it from?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.label(egui::RichText::new(display_name(&self.name)).strong());
                ui.label(egui::RichText::new(format!("Now under {}", self.current)).small().color(theme::text_muted()));
                ui.add_space(4.0);
                let field = ui.add(egui::TextEdit::singleline(&mut self.typed).hint_text("Game name").desired_width(f32::INFINITY));
                if std::mem::take(&mut self.focus) {
                    field.request_focus();
                }
                let typed = self.typed.trim().to_lowercase();
                let suggestions: Vec<&String> = known
                    .iter()
                    .filter(|game| typed.is_empty() || game.to_lowercase().contains(&typed))
                    .filter(|game| game.to_lowercase() != typed)
                    .take(SUGGESTIONS)
                    .collect();
                for game in suggestions {
                    if ui.selectable_label(false, game.as_str()).clicked() {
                        choice = GameChoice::Set(self.clip.clone(), Some(game.clone()));
                    }
                }
                ui.add_space(6.0);
                let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let ready = !self.typed.trim().is_empty();
                    if (ui.add_enabled(ready, theme::primary_button("Set")).clicked() || entered) && ready {
                        choice = GameChoice::Set(self.clip.clone(), Some(self.typed.trim().to_string()));
                    }
                    if self.chosen && ui.button("Use what Sonora found").clicked() {
                        choice = GameChoice::Set(self.clip.clone(), None);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = GameChoice::Closed;
                    }
                });
            });
        if !open {
            return GameChoice::Closed;
        }
        choice
    }
}
