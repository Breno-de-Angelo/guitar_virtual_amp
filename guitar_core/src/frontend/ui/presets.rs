use std::path::PathBuf;

use eframe::egui::Ui;

use crate::shared::{
    factory_presets::factory_presets,
    pedals::PedalDescription,
    preset::{self, Preset, PresetListing},
};

/// UI state for the preset save/load/rename/delete/import/export panel.
/// Owns the cached listing of user presets (re-read from disk on every
/// mutation) plus whatever transient text-entry state the panel needs.
pub struct PresetPanel {
    user_presets: Vec<PresetListing>,
    save_name: String,
    status: Option<String>,
    renaming: Option<(PathBuf, String)>,
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
            status: None,
            renaming: None,
            #[cfg(target_os = "android")]
            manual_path: String::new(),
        };
        panel.refresh();
        panel
    }

    fn refresh(&mut self) {
        self.user_presets = preset::user_preset_dir()
            .map(|dir| preset::list_presets(&dir))
            .unwrap_or_default();
    }

    fn current_preset(&self, pedals: &[PedalDescription]) -> Preset {
        Preset {
            name: self.save_name.trim().to_string(),
            author: None,
            pedals: pedals.to_vec(),
            tags: Vec::new(),
            song: None,
            artist: None,
        }
    }

    /// Renders the panel. Returns `Some(pedals)` the one time this frame that
    /// the user loaded a preset (factory or user-saved) into the live chain,
    /// so the caller can push it through `pedal_tx` like any other chain edit.
    pub fn ui(&mut self, ui: &mut Ui, current_pedals: &[PedalDescription]) -> Option<Vec<PedalDescription>> {
        let mut loaded_chain = None;

        ui.heading("Presets");

        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut self.save_name);
        });

        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                if self.save_name.trim().is_empty() {
                    self.status = Some("Enter a name before saving".to_string());
                } else {
                    match preset::user_preset_dir() {
                        Some(dir) => {
                            let preset = self.current_preset(current_pedals);
                            let path = dir.join(format!(
                                "{}.json",
                                preset::sanitize_file_stem(&preset.name)
                            ));
                            match preset::save_to_file(&preset, &path) {
                                Ok(()) => {
                                    self.status = Some(format!("Saved \"{}\"", preset.name));
                                    self.refresh();
                                }
                                Err(e) => self.status = Some(format!("Save failed: {e}")),
                            }
                        }
                        None => {
                            self.status =
                                Some("No writable preset directory on this platform".to_string())
                        }
                    }
                }
            }

            self.export_button(ui, current_pedals);
            if let Some(pedals) = self.import_button(ui) {
                loaded_chain = Some(pedals);
            }
        });

        if let Some(status) = &self.status {
            ui.label(status);
        }

        ui.separator();
        ui.label("Your Presets");
        if self.user_presets.is_empty() {
            ui.label("(none saved yet)");
        }
        let mut to_delete: Option<PathBuf> = None;
        let mut to_rename: Option<(PathBuf, String)> = None;
        for listing in self.user_presets.clone() {
            ui.horizontal(|ui| {
                let is_renaming = self
                    .renaming
                    .as_ref()
                    .is_some_and(|(path, _)| path == &listing.path);

                if is_renaming {
                    let (_, new_name) = self.renaming.as_mut().unwrap();
                    ui.text_edit_singleline(new_name);
                    if ui.button("✓").clicked() {
                        let new_name = self.renaming.take().unwrap().1;
                        to_rename = Some((listing.path.clone(), new_name));
                    }
                    if ui.button("✗").clicked() {
                        self.renaming = None;
                    }
                } else {
                    ui.label(&listing.name);
                    if ui.button("Load").clicked() {
                        if let Ok(preset) = preset::load_from_file(&listing.path) {
                            loaded_chain = Some(preset.pedals);
                        }
                    }
                    if ui.button("Rename").clicked() {
                        self.renaming = Some((listing.path.clone(), listing.name.clone()));
                    }
                    if ui.button("Delete").clicked() {
                        to_delete = Some(listing.path.clone());
                    }
                }
            });
        }

        if let Some(path) = to_delete {
            if let Err(e) = preset::delete_file(&path) {
                self.status = Some(format!("Delete failed: {e}"));
            }
            self.refresh();
        }

        if let Some((old_path, new_name)) = to_rename {
            if new_name.trim().is_empty() {
                self.status = Some("Enter a name before renaming".to_string());
            } else if let Ok(mut preset) = preset::load_from_file(&old_path) {
                preset.name = new_name.trim().to_string();
                let new_path = old_path.with_file_name(format!(
                    "{}.json",
                    preset::sanitize_file_stem(&preset.name)
                ));
                match preset::save_to_file(&preset, &new_path) {
                    Ok(()) => {
                        if new_path != old_path {
                            let _ = preset::delete_file(&old_path);
                        }
                        self.status = Some(format!("Renamed to \"{}\"", preset.name));
                    }
                    Err(e) => self.status = Some(format!("Rename failed: {e}")),
                }
                self.refresh();
            }
        }

        ui.separator();
        ui.label("Factory Presets");
        for preset in factory_presets() {
            ui.horizontal(|ui| {
                ui.label(&preset.name);
                if ui.button("Load").clicked() {
                    loaded_chain = Some(preset.pedals.clone());
                }
            });
        }

        loaded_chain
    }

    /// "Export current..." button: writes the live chain to a file picked via
    /// a native save dialog (desktop) or a manually entered path (Android,
    /// where `rfd` has no backend).
    #[cfg(not(target_os = "android"))]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalDescription]) {
        if ui.button("Export current...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_file_name(format!(
                    "{}.json",
                    preset::sanitize_file_stem(&self.save_name)
                ))
                .save_file()
            {
                let preset = self.current_preset(current_pedals);
                match preset::save_to_file(&preset, &path) {
                    Ok(()) => self.status = Some(format!("Exported to {}", path.display())),
                    Err(e) => self.status = Some(format!("Export failed: {e}")),
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalDescription]) {
        ui.label("Export path:");
        ui.text_edit_singleline(&mut self.manual_path);
        if ui.button("Export current").clicked() && !self.manual_path.trim().is_empty() {
            let preset = self.current_preset(current_pedals);
            let path = PathBuf::from(self.manual_path.trim());
            match preset::save_to_file(&preset, &path) {
                Ok(()) => self.status = Some(format!("Exported to {}", path.display())),
                Err(e) => self.status = Some(format!("Export failed: {e}")),
            }
        }
    }

    /// "Import from file..." button: loads a preset from a file picked via a
    /// native open dialog (desktop) or a manually entered path (Android).
    /// Returns the imported pedal chain if the user picked/entered a valid file.
    #[cfg(not(target_os = "android"))]
    fn import_button(&mut self, ui: &mut Ui) -> Option<Vec<PedalDescription>> {
        if ui.button("Import from file...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                return match preset::load_from_file(&path) {
                    Ok(preset) => {
                        self.status = Some(format!("Imported \"{}\"", preset.name));
                        Some(preset.pedals)
                    }
                    Err(e) => {
                        self.status = Some(format!("Import failed: {e}"));
                        None
                    }
                };
            }
        }
        None
    }

    #[cfg(target_os = "android")]
    fn import_button(&mut self, ui: &mut Ui) -> Option<Vec<PedalDescription>> {
        if ui.button("Import from path").clicked() && !self.manual_path.trim().is_empty() {
            let path = PathBuf::from(self.manual_path.trim());
            return match preset::load_from_file(&path) {
                Ok(preset) => {
                    self.status = Some(format!("Imported \"{}\"", preset.name));
                    Some(preset.pedals)
                }
                Err(e) => {
                    self.status = Some(format!("Import failed: {e}"));
                    None
                }
            };
        }
        None
    }
}
