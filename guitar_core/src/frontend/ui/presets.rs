//! "Presets" page: save the chain (with optional song/artist metadata for
//! the tab player's tone suggestions), load, rename, delete, import and
//! export presets, search them, and arrange a setlist for quick switching
//! (top-bar arrows, PageUp/PageDown, MIDI program changes).

use std::path::PathBuf;

use eframe::egui::{self, RichText, Ui};

use crate::frontend::style::{self, Palette};
use crate::shared::{
    factory_presets::factory_presets,
    pedals::PedalInstance,
    preset::{self, Preset, PresetListing},
    settings::{PresetRef, UiPrefs},
};

/// Loads the preset a reference points to.
pub fn load_preset_ref(preset_ref: &PresetRef) -> Option<Preset> {
    match preset_ref {
        PresetRef::User(path) => preset::load_from_file(path).ok(),
        PresetRef::Factory(name) => factory_presets().into_iter().find(|p| &p.name == name),
    }
}

/// Display name of a reference without reading the file when possible.
pub fn preset_ref_name(preset_ref: &PresetRef) -> String {
    match preset_ref {
        PresetRef::Factory(name) => name.clone(),
        PresetRef::User(path) => load_preset_ref(preset_ref)
            .map(|p| p.name)
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
    }
}

/// The order next/previous preset walks: the setlist, or when it's empty,
/// every user preset then every factory preset.
pub fn preset_order(prefs: &UiPrefs, user: &[PresetListing]) -> Vec<PresetRef> {
    if !prefs.setlist.is_empty() {
        return prefs.setlist.clone();
    }
    user.iter()
        .map(|l| PresetRef::User(l.path.clone()))
        .chain(factory_presets().into_iter().map(|p| PresetRef::Factory(p.name)))
        .collect()
}

/// The entry `step` places after `current` in `order`, wrapping around.
pub fn step_preset(order: &[PresetRef], current: Option<&PresetRef>, step: isize) -> Option<PresetRef> {
    if order.is_empty() {
        return None;
    }
    let len = order.len() as isize;
    let index = match current.and_then(|c| order.iter().position(|r| r == c)) {
        Some(i) => (i as isize + step).rem_euclid(len),
        None if step >= 0 => 0,
        None => len - 1,
    };
    order.get(index as usize).cloned()
}

/// A preset the user picked this frame.
pub struct LoadedPreset {
    pub pedals: Vec<PedalInstance>,
    pub preset_ref: PresetRef,
    pub name: String,
}

/// UI state for the preset page. Owns the cached listing of user presets
/// (re-read from disk on every mutation) plus transient text-entry state.
pub struct PresetPanel {
    user_presets: Vec<PresetListing>,
    save_name: String,
    save_song: String,
    save_artist: String,
    search: String,
    status: Option<(String, bool)>,
    renaming: Option<(PathBuf, String)>,
    confirm_delete: Option<PathBuf>,
    confirm_overwrite: Option<PathBuf>,
    /// Manual path entry, used in place of a native file dialog on platforms
    /// (Android) where `rfd` has no backend.
    #[cfg(target_os = "android")]
    manual_path: String,
}

impl Default for PresetPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetPanel {
    pub fn new() -> Self {
        let mut panel = Self {
            user_presets: Vec::new(),
            save_name: String::new(),
            save_song: String::new(),
            save_artist: String::new(),
            search: String::new(),
            status: None,
            renaming: None,
            confirm_delete: None,
            confirm_overwrite: None,
            #[cfg(target_os = "android")]
            manual_path: String::new(),
        };
        panel.refresh();
        panel
    }

    pub fn refresh(&mut self) {
        self.user_presets = preset::user_preset_dir()
            .map(|dir| preset::list_presets(&dir))
            .unwrap_or_default();
    }

    pub fn user_presets(&self) -> &[PresetListing] {
        &self.user_presets
    }

    /// Prefills the save form (e.g. with the name of the loaded preset).
    pub fn set_save_name(&mut self, name: &str) {
        self.save_name = name.to_string();
    }

    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((text.into(), error));
    }

    fn current_preset(&self, pedals: &[PedalInstance]) -> Preset {
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        Preset {
            name: self.save_name.trim().to_string(),
            author: None,
            pedals: pedals.to_vec(),
            tags: Vec::new(),
            song: opt(&self.save_song),
            artist: opt(&self.save_artist),
        }
    }

    /// Saves the chain under the name in the form. Returns the saved file.
    pub fn save(&mut self, pedals: &[PedalInstance], overwrite: bool) -> Option<PathBuf> {
        if self.save_name.trim().is_empty() {
            self.set_status("Enter a name before saving", true);
            return None;
        }
        let Some(dir) = preset::user_preset_dir() else {
            self.set_status("No writable preset directory on this platform", true);
            return None;
        };
        let preset = self.current_preset(pedals);
        let path = dir.join(format!("{}.json", preset::sanitize_file_stem(&preset.name)));
        if path.exists() && !overwrite {
            self.confirm_overwrite = Some(path);
            return None;
        }
        self.confirm_overwrite = None;
        match preset::save_to_file(&preset, &path) {
            Ok(()) => {
                self.set_status(format!("Saved \"{}\"", preset.name), false);
                self.refresh();
                Some(path)
            }
            Err(e) => {
                self.set_status(format!("Save failed: {e}"), true);
                None
            }
        }
    }

    /// Renders the page. Returns the preset the user loaded this frame, and
    /// whether `prefs` (the setlist) changed.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        current_pedals: &[PedalInstance],
        current: Option<&PresetRef>,
        prefs: &mut UiPrefs,
    ) -> (Option<LoadedPreset>, bool) {
        let palette = Palette::of(ui.ctx());
        let mut loaded = None;
        let mut prefs_changed = false;

        egui::ScrollArea::vertical().id_salt("presets_scroll").auto_shrink([false, false]).show(ui, |ui| {
            // Save form.
            style::card_frame(&palette).show(ui, |ui| {
                ui.set_width(ui.available_width());
                style::section_title(ui, "Save the current chain");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Name:");
                    let name = ui.add(egui::TextEdit::singleline(&mut self.save_name).hint_text("My crunch").desired_width(200.0));
                    let enter = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("💾 Save").clicked() || enter {
                        self.save(current_pedals, false);
                    }
                    self.export_button(ui, current_pedals);
                    if let Some(found) = self.import_button(ui) {
                        loaded = Some(found);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Song:");
                    ui.add(egui::TextEdit::singleline(&mut self.save_song).hint_text("optional").desired_width(160.0));
                    ui.label("Artist:");
                    ui.add(egui::TextEdit::singleline(&mut self.save_artist).hint_text("optional").desired_width(160.0));
                })
                .response
                .on_hover_text("The Tabs page suggests this preset when you open a tab of this song or artist");
                if let Some(path) = self.confirm_overwrite.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(palette.warn, format!("\"{}\" already exists.", self.save_name.trim()));
                        if ui.button("Overwrite").clicked() {
                            self.save(current_pedals, true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_overwrite = None;
                        }
                        let _ = path;
                    });
                }
                if let Some((status, error)) = &self.status {
                    ui.colored_label(if *error { palette.danger } else { palette.text_dim }, status);
                }
            });
            ui.add_space(style::space::M);

            ui.horizontal(|ui| {
                ui.label("🔍");
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search presets").desired_width(240.0));
                if !self.search.is_empty() && ui.small_button("✖").clicked() {
                    self.search.clear();
                }
            });
            let query = self.search.trim().to_lowercase();
            let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
            ui.add_space(style::space::S);

            // Setlist.
            if !prefs.setlist.is_empty() {
                style::section_title(ui, "Setlist");
                ui.label(
                    RichText::new("◀ ▶ in the top bar, PageUp/PageDown and MIDI program changes walk this list.")
                        .small()
                        .color(palette.text_dim),
                );
                let mut action: Option<(usize, isize)> = None;
                for (i, entry) in prefs.setlist.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{:>2}.", i + 1)).monospace().color(palette.text_dim));
                        let name = preset_ref_name(entry);
                        let is_current = current == Some(entry);
                        let text = if is_current { RichText::new(&name).strong().color(palette.accent) } else { RichText::new(&name) };
                        if ui.selectable_label(is_current, text).on_hover_text("Load").clicked()
                            && let Some(p) = load_preset_ref(entry)
                        {
                            loaded = Some(LoadedPreset { pedals: p.pedals, preset_ref: entry.clone(), name });
                        }
                        if ui.small_button("⬆").clicked() {
                            action = Some((i, -1));
                        }
                        if ui.small_button("⬇").clicked() {
                            action = Some((i, 1));
                        }
                        if ui.small_button("✖").on_hover_text("Remove from the setlist").clicked() {
                            action = Some((i, 0));
                        }
                    });
                }
                if let Some((i, dir)) = action {
                    if dir == 0 {
                        prefs.setlist.remove(i);
                    } else {
                        let j = (i as isize + dir).clamp(0, prefs.setlist.len() as isize - 1) as usize;
                        prefs.setlist.swap(i, j);
                    }
                    prefs_changed = true;
                }
                ui.add_space(style::space::M);
            }

            // User presets.
            style::section_title(ui, "Your presets");
            if self.user_presets.is_empty() {
                ui.label(RichText::new("None saved yet. Build a chain and save it above.").color(palette.text_dim));
            }
            let mut to_delete: Option<PathBuf> = None;
            let mut to_rename: Option<(PathBuf, String)> = None;
            for listing in self.user_presets.clone() {
                if !matches(&listing.name) {
                    continue;
                }
                let preset_ref = PresetRef::User(listing.path.clone());
                ui.horizontal(|ui| {
                    prefs_changed |= setlist_star(ui, prefs, &preset_ref);
                    let is_renaming = self.renaming.as_ref().is_some_and(|(path, _)| path == &listing.path);
                    if is_renaming {
                        let (_, new_name) = self.renaming.as_mut().expect("checked above");
                        ui.text_edit_singleline(new_name);
                        if ui.button("✓").clicked() {
                            let new_name = self.renaming.take().expect("checked above").1;
                            to_rename = Some((listing.path.clone(), new_name));
                        }
                        if ui.button("✗").clicked() {
                            self.renaming = None;
                        }
                    } else if self.confirm_delete.as_ref() == Some(&listing.path) {
                        ui.colored_label(palette.danger, format!("Delete \"{}\"?", listing.name));
                        if ui.button("Delete").clicked() {
                            to_delete = Some(listing.path.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete = None;
                        }
                    } else {
                        let is_current = current == Some(&preset_ref);
                        let text = if is_current {
                            RichText::new(&listing.name).strong().color(palette.accent)
                        } else {
                            RichText::new(&listing.name)
                        };
                        if ui.selectable_label(is_current, text).on_hover_text("Load").clicked()
                            && let Ok(p) = preset::load_from_file(&listing.path)
                        {
                            self.save_song = p.song.clone().unwrap_or_default();
                            self.save_artist = p.artist.clone().unwrap_or_default();
                            self.save_name = p.name.clone();
                            loaded = Some(LoadedPreset { pedals: p.pedals, preset_ref, name: p.name });
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                                self.confirm_delete = Some(listing.path.clone());
                            }
                            if ui.small_button("✏").on_hover_text("Rename").clicked() {
                                self.renaming = Some((listing.path.clone(), listing.name.clone()));
                            }
                        });
                    }
                });
            }

            if let Some(path) = to_delete {
                if let Err(e) = preset::delete_file(&path) {
                    self.set_status(format!("Delete failed: {e}"), true);
                }
                prefs.setlist.retain(|r| r != &PresetRef::User(path.clone()));
                prefs_changed = true;
                self.confirm_delete = None;
                self.refresh();
            }

            if let Some((old_path, new_name)) = to_rename {
                if new_name.trim().is_empty() {
                    self.set_status("Enter a name before renaming", true);
                } else if let Ok(mut preset) = preset::load_from_file(&old_path) {
                    preset.name = new_name.trim().to_string();
                    let new_path =
                        old_path.with_file_name(format!("{}.json", preset::sanitize_file_stem(&preset.name)));
                    match preset::save_to_file(&preset, &new_path) {
                        Ok(()) => {
                            if new_path != old_path {
                                let _ = preset::delete_file(&old_path);
                                for entry in prefs.setlist.iter_mut() {
                                    if *entry == PresetRef::User(old_path.clone()) {
                                        *entry = PresetRef::User(new_path.clone());
                                        prefs_changed = true;
                                    }
                                }
                            }
                            self.set_status(format!("Renamed to \"{}\"", preset.name), false);
                        }
                        Err(e) => self.set_status(format!("Rename failed: {e}"), true),
                    }
                    self.refresh();
                }
            }

            ui.add_space(style::space::M);
            style::section_title(ui, "Factory presets");
            ui.horizontal_wrapped(|ui| {
                for preset in factory_presets() {
                    if !matches(&preset.name) {
                        continue;
                    }
                    let preset_ref = PresetRef::Factory(preset.name.clone());
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            prefs_changed |= setlist_star(ui, prefs, &preset_ref);
                            let is_current = current == Some(&preset_ref);
                            let text = if is_current {
                                RichText::new(&preset.name).strong().color(palette.accent)
                            } else {
                                RichText::new(&preset.name)
                            };
                            let chain: Vec<&str> = preset.pedals.iter().map(|p| p.description.name()).collect();
                            if ui.selectable_label(is_current, text).on_hover_text(chain.join(" → ")).clicked() {
                                loaded = Some(LoadedPreset {
                                    pedals: preset.pedals.clone(),
                                    preset_ref,
                                    name: preset.name.clone(),
                                });
                            }
                        });
                    });
                }
            });
        });

        (loaded, prefs_changed)
    }

    /// "Export current..." button: writes the live chain to a file picked via
    /// a native save dialog (desktop) or a manually entered path (Android,
    /// where `rfd` has no backend).
    #[cfg(not(target_os = "android"))]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalInstance]) {
        if ui.button("Export...").on_hover_text("Write the chain to a .json file").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_file_name(format!("{}.json", preset::sanitize_file_stem(&self.save_name)))
                .save_file()
        {
            let preset = self.current_preset(current_pedals);
            match preset::save_to_file(&preset, &path) {
                Ok(()) => self.set_status(format!("Exported to {}", path.display()), false),
                Err(e) => self.set_status(format!("Export failed: {e}"), true),
            }
        }
    }

    #[cfg(target_os = "android")]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalInstance]) {
        ui.label("Export path:");
        ui.text_edit_singleline(&mut self.manual_path);
        if ui.button("Export current").clicked() && !self.manual_path.trim().is_empty() {
            let preset = self.current_preset(current_pedals);
            let path = PathBuf::from(self.manual_path.trim());
            match preset::save_to_file(&preset, &path) {
                Ok(()) => self.set_status(format!("Exported to {}", path.display()), false),
                Err(e) => self.set_status(format!("Export failed: {e}"), true),
            }
        }
    }

    /// "Import..." button: loads a preset from a file picked via a native
    /// open dialog (desktop) or a manually entered path (Android).
    #[cfg(not(target_os = "android"))]
    fn import_button(&mut self, ui: &mut Ui) -> Option<LoadedPreset> {
        if ui.button("Import...").on_hover_text("Load a preset .json file").clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file()
        {
            return self.import_from(path);
        }
        None
    }

    #[cfg(target_os = "android")]
    fn import_button(&mut self, ui: &mut Ui) -> Option<LoadedPreset> {
        if ui.button("Import from path").clicked() && !self.manual_path.trim().is_empty() {
            let path = PathBuf::from(self.manual_path.trim());
            return self.import_from(path);
        }
        None
    }

    fn import_from(&mut self, path: PathBuf) -> Option<LoadedPreset> {
        match preset::load_from_file(&path) {
            Ok(preset) => {
                self.set_status(format!("Imported \"{}\"", preset.name), false);
                Some(LoadedPreset { pedals: preset.pedals, preset_ref: PresetRef::User(path), name: preset.name })
            }
            Err(e) => {
                self.set_status(format!("Import failed: {e}"), true);
                None
            }
        }
    }
}

/// ★/☆ toggle adding/removing `preset_ref` from the setlist.
fn setlist_star(ui: &mut Ui, prefs: &mut UiPrefs, preset_ref: &PresetRef) -> bool {
    let palette = Palette::of(ui.ctx());
    let in_setlist = prefs.setlist.contains(preset_ref);
    let (icon, color, hover) = if in_setlist {
        ("★", palette.accent, "In the setlist (click to remove)")
    } else {
        ("☆", palette.text_dim, "Add to the setlist")
    };
    if ui.add(egui::Button::new(RichText::new(icon).color(color)).frame(false)).on_hover_text(hover).clicked() {
        if in_setlist {
            prefs.setlist.retain(|r| r != preset_ref);
        } else {
            prefs.setlist.push(preset_ref.clone());
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_and_starts_at_the_ends() {
        let order = vec![PresetRef::Factory("A".into()), PresetRef::Factory("B".into()), PresetRef::Factory("C".into())];
        assert_eq!(step_preset(&order, Some(&order[2]), 1), Some(order[0].clone()));
        assert_eq!(step_preset(&order, Some(&order[0]), -1), Some(order[2].clone()));
        assert_eq!(step_preset(&order, None, 1), Some(order[0].clone()));
        assert_eq!(step_preset(&order, None, -1), Some(order[2].clone()));
        assert_eq!(step_preset(&[], None, 1), None);
    }

    #[test]
    fn order_is_the_setlist_or_everything() {
        let mut prefs = UiPrefs::default();
        let all = preset_order(&prefs, &[]);
        assert_eq!(all.len(), factory_presets().len());
        prefs.setlist = vec![PresetRef::Factory("Rock Lead".into())];
        assert_eq!(preset_order(&prefs, &[]), prefs.setlist);
        assert!(load_preset_ref(&PresetRef::Factory("Rock Lead".into())).is_some());
        assert_eq!(preset_ref_name(&PresetRef::Factory("X".into())), "X");
    }

    #[test]
    fn page_renders_and_stars_toggle_the_setlist() {
        let ctx = egui::Context::default();
        let mut panel = PresetPanel::new();
        let mut prefs = UiPrefs::default();
        let star = PresetRef::Factory("Rock Lead".into());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (loaded, changed) = panel.ui(ui, &[], None, &mut prefs);
                assert!(loaded.is_none());
                assert!(!changed);
                assert!(!setlist_star_toggled(ui, &mut prefs, &star));
            });
        });
    }

    /// `setlist_star` only acts on a click, which a headless frame can't produce.
    fn setlist_star_toggled(ui: &mut Ui, prefs: &mut UiPrefs, r: &PresetRef) -> bool {
        setlist_star(ui, prefs, r)
    }
}
