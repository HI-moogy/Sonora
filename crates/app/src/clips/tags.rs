//! Clip tags on the clips screen: each clip's right-click menu (tick its
//! tags, make one in a color you pick, recolor or delete one), the tags on
//! its card, the filter above the clips, and renaming in the settings.

use std::collections::HashMap;
use std::path::Path;

use common::{ClipTag, ClipTags, Rgb, TagError};
use eframe::egui;

use crate::theme;

/// What you're doing with the tags, from frame to frame.
#[derive(Default)]
pub struct TagUi {
    /// Show only clips with this tag.
    pub filter: Option<String>,
    /// A tag being typed in a clip's menu.
    new_name: String,
    /// The color it gets; `None`: the first one no tag has.
    new_color: Option<Rgb>,
    /// Whose colors are out to pick from.
    picking: Option<Picking>,
    /// A tag on clips about to be deleted, waiting for you to confirm.
    deleting: Option<String>,
    /// In the settings: a tag being renamed, and the name typed so far.
    renaming: Option<(String, String)>,
    /// Why that name can't be used.
    rename_note: Option<String>,
}

#[derive(PartialEq)]
enum Picking {
    Tag(String),
    New,
}

/// How many of the listed clips have each tag.
pub type TagCounts = HashMap<String, usize>;

impl TagUi {
    /// A clip's right-click menu: each tag with a box to tick, its color
    /// (click to change it) and ✕ to delete it, then a new one to make.
    /// Returns whether your tags changed.
    pub fn menu(&mut self, ui: &mut egui::Ui, clip: &Path, tags: &mut ClipTags, counts: &TagCounts) -> bool {
        ui.set_width(220.0);
        let mut changed = false;
        ui.label(egui::RichText::new("Tags").small().color(theme::text_muted()));
        for tag in tags.tags.clone() {
            ui.horizontal(|ui| {
                let mut on = tags.has(clip, &tag.name);
                if ui.checkbox(&mut on, &tag.name).changed() {
                    tags.set(clip, &tag.name, on);
                    changed = true;
                }
                if swatch(ui, tag.color, false).on_hover_text("Change its color").clicked() {
                    self.toggle_picking(Picking::Tag(tag.name.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("✕").frame(false)).on_hover_text("Delete this tag").clicked() {
                        changed |= self.delete(tags, &tag.name, counts);
                    }
                });
            });
            changed |= self.tag_extras(ui, tags, &tag, counts);
        }

        // Across the whole menu, Add lined up with the ✕s above it.
        ui.horizontal(|ui| {
            let color = self.new_color.unwrap_or_else(|| unused_color(tags));
            if swatch(ui, color, false).on_hover_text("Its color").clicked() {
                self.toggle_picking(Picking::New);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let add = ui.add_enabled(!self.new_name.trim().is_empty(), egui::Button::new("Add")).clicked();
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.new_name)
                        .hint_text("New tag")
                        .desired_width(ui.available_width())
                        .char_limit(ClipTags::MAX_LEN),
                );
                let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if (add || entered) && !self.new_name.trim().is_empty() {
                    // A tag you already have just goes on the clip.
                    if let Ok(tag) | Err(TagError::Exists(tag)) = tags.add_tag(&self.new_name, color) {
                        tags.set(clip, &tag, true);
                        changed = true;
                    }
                    self.new_name.clear();
                    self.new_color = None;
                    self.picking = None;
                }
            });
        });
        if self.picking == Some(Picking::New) {
            let current = self.new_color.unwrap_or_else(|| unused_color(tags));
            if let Some(color) = palette(ui, current) {
                self.new_color = Some(color);
                self.picking = None;
            }
        }
        changed
    }

    /// The tag filter: any tag, or one of yours with how many clips have it.
    pub fn filter(&mut self, ui: &mut egui::Ui, tags: &ClipTags, counts: &TagCounts) {
        if self.filter.as_ref().is_some_and(|filter| !tags.tags.iter().any(|tag| &tag.name == filter)) {
            self.filter = None;
        }
        let selected = self.filter.clone().unwrap_or_else(|| "Any".to_string());
        egui::ComboBox::from_id_salt("clip-tag").width(150.0).selected_text(selected).show_ui(ui, |ui| {
            if ui.selectable_label(self.filter.is_none(), "Any").clicked() {
                self.filter = None;
            }
            for tag in &tags.tags {
                ui.horizontal(|ui| {
                    swatch(ui, tag.color, false);
                    let label = format!("{} ({})", tag.name, counts.get(&tag.name).copied().unwrap_or(0));
                    if ui.selectable_label(self.filter.as_ref() == Some(&tag.name), label).clicked() {
                        self.filter = Some(tag.name.clone());
                    }
                });
            }
        });
    }

    /// Every tag, to rename, recolor or delete. Returns whether your tags
    /// changed.
    pub fn settings(&mut self, ui: &mut egui::Ui, tags: &mut ClipTags, counts: &TagCounts) -> bool {
        if tags.tags.is_empty() {
            ui.label(egui::RichText::new("No tags yet. Right-click a clip to make one.").color(theme::text_muted()));
            return false;
        }
        let mut changed = false;
        for tag in tags.tags.clone() {
            ui.horizontal(|ui| {
                if swatch(ui, tag.color, false).on_hover_text("Change its color").clicked() {
                    self.toggle_picking(Picking::Tag(tag.name.clone()));
                }
                match &mut self.renaming {
                    Some((renamed, typed)) if *renamed == tag.name => {
                        let field = ui.add(egui::TextEdit::singleline(typed).desired_width(140.0).char_limit(ClipTags::MAX_LEN));
                        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if ui.small_button("Save").clicked() || entered {
                            match tags.rename_tag(&tag.name, typed) {
                                Ok(name) => {
                                    if self.filter.as_ref() == Some(&tag.name) {
                                        self.filter = Some(name);
                                    }
                                    self.stop_renaming();
                                    changed = true;
                                }
                                Err(TagError::Exists(other)) => {
                                    self.rename_note = Some(format!("There's a tag called {other} already."));
                                }
                                // Nothing typed: nothing to do.
                                Err(_) => self.stop_renaming(),
                            }
                        } else if ui.small_button("Cancel").clicked() || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                            self.stop_renaming();
                        }
                    }
                    _ => {
                        ui.label(&tag.name);
                        let count = counts.get(&tag.name).copied().unwrap_or(0);
                        ui.weak(format!("{count} clip{}", if count == 1 { "" } else { "s" }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Delete").clicked() {
                                changed |= self.delete(tags, &tag.name, counts);
                            }
                            if ui.small_button("Rename").clicked() {
                                self.renaming = Some((tag.name.clone(), tag.name.clone()));
                                self.rename_note = None;
                            }
                        });
                    }
                }
            });
            if self.renaming.as_ref().is_some_and(|(renamed, _)| *renamed == tag.name) {
                if let Some(note) = &self.rename_note {
                    ui.colored_label(theme::danger(), note);
                }
            }
            changed |= self.tag_extras(ui, tags, &tag, counts);
        }
        changed
    }

    /// Under a tag's row: its colors to pick from, or the question before
    /// deleting it.
    fn tag_extras(&mut self, ui: &mut egui::Ui, tags: &mut ClipTags, tag: &ClipTag, counts: &TagCounts) -> bool {
        let mut changed = false;
        if self.picking.as_ref() == Some(&Picking::Tag(tag.name.clone())) {
            if let Some(color) = palette(ui, tag.color) {
                tags.set_color(&tag.name, color);
                self.picking = None;
                changed = true;
            }
        }
        if self.deleting.as_ref() == Some(&tag.name) {
            let count = counts.get(&tag.name).copied().unwrap_or(0);
            ui.horizontal(|ui| {
                ui.weak(format!("Take {} off {count} clip{}?", tag.name, if count == 1 { "" } else { "s" }));
                if ui.add(egui::Button::new(egui::RichText::new("Delete").color(theme::danger())).small()).clicked() {
                    self.remove(tags, &tag.name);
                    changed = true;
                }
                if ui.small_button("Keep").clicked() {
                    self.deleting = None;
                }
            });
        }
        changed
    }

    /// Deletes a tag no clip has; one on clips asks first. Returns whether
    /// it was deleted.
    fn delete(&mut self, tags: &mut ClipTags, tag: &str, counts: &TagCounts) -> bool {
        if counts.get(tag).copied().unwrap_or(0) == 0 {
            self.remove(tags, tag);
            true
        } else {
            self.deleting = Some(tag.to_string());
            false
        }
    }

    fn remove(&mut self, tags: &mut ClipTags, tag: &str) {
        tags.remove_tag(tag);
        if self.filter.as_deref() == Some(tag) {
            self.filter = None;
        }
        self.deleting = None;
        self.picking = None;
    }

    fn stop_renaming(&mut self) {
        self.renaming = None;
        self.rename_note = None;
    }

    fn toggle_picking(&mut self, picking: Picking) {
        self.picking = if self.picking.as_ref() == Some(&picking) { None } else { Some(picking) };
    }
}

/// A clip's tags along the bottom of its thumbnail, in their colors, as
/// many as fit.
pub fn chips(ui: &egui::Ui, thumbnail: egui::Rect, tags: &[&ClipTag]) {
    let mut left = thumbnail.left() + 6.0;
    for tag in tags {
        let fill = theme::color(tag.color);
        let text = theme::text_on(fill);
        let galley = ui.painter().layout_no_wrap(tag.name.clone(), egui::FontId::proportional(11.0), text);
        let size = galley.size() + egui::vec2(10.0, 4.0);
        let chip = egui::Rect::from_min_size(egui::pos2(left, thumbnail.bottom() - 6.0 - size.y), size);
        if chip.right() > thumbnail.right() - 6.0 {
            break;
        }
        ui.painter().rect_filled(chip, 4.0, fill);
        ui.painter().galley(chip.min + egui::vec2(5.0, 2.0), galley, text);
        left = chip.right() + 4.0;
    }
}

/// How many of `clips` have each tag.
pub fn counts<'a>(tags: &ClipTags, clips: impl IntoIterator<Item = &'a Path>) -> TagCounts {
    let mut counts = TagCounts::new();
    for clip in clips {
        for tag in tags.of(clip) {
            *counts.entry(tag.name.clone()).or_default() += 1;
        }
    }
    counts
}

/// A tag's color as a little square; `outlined` marks the one picked.
fn swatch(ui: &mut egui::Ui, color: Rgb, outlined: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 3.0, theme::color(color));
    if outlined || response.hovered() {
        ui.painter().rect_stroke(rect.expand(1.5), 4.0, egui::Stroke::new(1.5, theme::text()), egui::StrokeKind::Outside);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The colors to pick from, `current` marked; returns the one clicked.
fn palette(ui: &mut egui::Ui, current: Rgb) -> Option<Rgb> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for color in theme::TAG_COLORS {
            if swatch(ui, color, color == current).clicked() {
                picked = Some(color);
            }
        }
    });
    picked
}

/// The first color to pick from that no tag has (or the first, if all
/// are taken), so a new tag stands apart.
fn unused_color(tags: &ClipTags) -> Rgb {
    theme::TAG_COLORS
        .into_iter()
        .find(|color| !tags.tags.iter().any(|tag| tag.color == *color))
        .unwrap_or(theme::TAG_COLORS[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_tag_gets_a_color_no_tag_has() {
        let mut tags = ClipTags::default();
        assert_eq!(unused_color(&tags), theme::TAG_COLORS[0]);
        tags.add_tag("Funny", theme::TAG_COLORS[0]).unwrap();
        tags.add_tag("Cool", theme::TAG_COLORS[2]).unwrap();
        assert_eq!(unused_color(&tags), theme::TAG_COLORS[1]);
    }

    #[test]
    fn counts_only_the_clips_listed() {
        let mut tags = ClipTags::default();
        tags.add_tag("Funny", theme::TAG_COLORS[0]).unwrap();
        tags.set(Path::new("a.mp4"), "Funny", true);
        tags.set(Path::new("gone.mp4"), "Funny", true);
        let counts = counts(&tags, [Path::new("a.mp4"), Path::new("b.mp4")]);
        assert_eq!(counts.get("Funny"), Some(&1));
    }
}
