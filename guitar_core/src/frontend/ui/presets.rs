//! "Presets" page: save the chain (with optional song/artist metadata for
//! the tab player's tone suggestions), load, rename, delete, import and
//! export presets, search them, and arrange a setlist for quick switching
//! (top-bar arrows, PageUp/PageDown, MIDI program changes).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui::{self, RichText, Ui};

use crate::frontend::style::{self, Palette};
use crate::shared::i18n::tr;
use crate::trf;
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

/// A user preset file that was saved, renamed or deleted from the page.
#[derive(Clone, Debug, PartialEq)]
pub enum PresetFileEvent {
    /// The live chain was saved to this file under this name.
    Saved(PathBuf, String),
    Renamed { from: PathBuf, to: PathBuf, name: String },
    Deleted(PathBuf),
}

/// A preset the user picked this frame.
pub struct LoadedPreset {
    pub pedals: Vec<PedalInstance>,
    pub preset_ref: PresetRef,
    pub name: String,
}

/// A user preset file as last read from disk.
struct SavedCopy {
    path: PathBuf,
    modified: Option<SystemTime>,
    preset: Option<Preset>,
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
    /// Whether the "Save current chain" form is shown.
    save_open: bool,
    /// Give the name field the keyboard focus on the next frame.
    focus_save_name: bool,
    /// The user preset file the chain was loaded from, as it is on disk, to
    /// tell whether the chain has unsaved edits. Re-read when the file's
    /// modification time changes.
    saved_copy: Option<SavedCopy>,
    /// File changes the app must reflect in its "current preset"
    /// (see [`PresetPanel::take_file_events`]).
    events: Vec<PresetFileEvent>,
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
            save_open: false,
            focus_save_name: false,
            saved_copy: None,
            events: Vec::new(),
            #[cfg(target_os = "android")]
            manual_path: String::new(),
        };
        panel.refresh();
        panel
    }

    /// Saves, renames and deletes since the last call.
    pub fn take_file_events(&mut self) -> Vec<PresetFileEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn refresh(&mut self) {
        self.saved_copy = None;
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

    /// Shows the "Save current chain" form with the name field focused (for
    /// Ctrl+S when the chain isn't a user preset yet).
    pub fn open_save_form(&mut self) {
        self.save_open = true;
        self.status = None;
        self.focus_save_name = true;
    }

    /// The user preset at `path` as saved on disk, cached until the file
    /// changes.
    fn saved_copy(&mut self, path: &Path) -> Option<&Preset> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let stale = self.saved_copy.as_ref().is_none_or(|c| c.path != path || c.modified != modified);
        if stale {
            self.saved_copy =
                Some(SavedCopy { path: path.to_path_buf(), modified, preset: preset::load_from_file(path).ok() });
        }
        self.saved_copy.as_ref().and_then(|c| c.preset.as_ref())
    }

    /// Overwrites the chain of the user preset at `path`, keeping its name,
    /// song and artist.
    fn save_changes(&mut self, path: &Path, pedals: &[PedalInstance]) {
        let result = preset::load_from_file(path).and_then(|mut existing| {
            existing.pedals = pedals.to_vec();
            preset::save_to_file(&existing, path).map(|()| existing.name)
        });
        match result {
            Ok(name) => {
                self.set_status(trf!("Saved \"{}\"", "\"{}\" salvo", name), false);
                self.events.push(PresetFileEvent::Saved(path.to_path_buf(), name));
                self.refresh();
            }
            Err(e) => self.set_status(trf!("Save failed: {e}", "Falha ao salvar: {e}"), true),
        }
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
            self.set_status(tr("Enter a name before saving", "Digite um nome antes de salvar"), true);
            return None;
        }
        let Some(dir) = preset::user_preset_dir() else {
            self.set_status(tr("No writable preset directory on this platform", "Sem pasta gravável para presets nesta plataforma"), true);
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
                self.set_status(trf!("Saved \"{}\"", "\"{}\" salvo", preset.name), false);
                self.events.push(PresetFileEvent::Saved(path.clone(), preset.name.clone()));
                self.refresh();
                Some(path)
            }
            Err(e) => {
                self.set_status(trf!("Save failed: {e}", "Falha ao salvar: {e}"), true);
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

        // "Save changes to <name>" when the chain came from a user preset
        // and was edited since.
        let edited_user_preset = match current {
            Some(PresetRef::User(path)) => self
                .saved_copy(path)
                .filter(|saved| saved.pedals.as_slice() != current_pedals)
                .map(|saved| (path.clone(), saved.name.clone())),
            _ => None,
        };

        egui::ScrollArea::vertical().id_salt("presets_scroll").auto_shrink([false, false]).show(ui, |ui| {
            // Page actions and search.
            ui.horizontal_wrapped(|ui| {
                let save_button = egui::Button::new(tr("💾 Save current chain...", "💾 Salvar cadeia atual..."))
                    .selected(self.save_open);
                if ui.add(save_button).on_hover_text(tr("Save the chain as a new preset", "Salva a cadeia como um novo preset")).clicked() {
                    if self.save_open {
                        self.close_save_form();
                    } else {
                        self.open_save_form();
                    }
                }
                if let Some((path, name)) = &edited_user_preset {
                    let label = trf!("💾 Save changes to {}", "💾 Salvar alterações em {}", ellipsize(name, 28));
                    if ui.button(label).on_hover_text(tr("Overwrite this preset with the current chain (Ctrl+S)", "Substitui este preset pela cadeia atual (Ctrl+S)")).clicked() {
                        self.save_changes(path, current_pedals);
                    }
                }
                self.export_button(ui, current_pedals);
                if let Some(found) = self.import_button(ui) {
                    loaded = Some(found);
                }
                ui.add_space(style::space::M);
                ui.add(text_field(ui, &mut self.search, tr("🔍 Search presets", "🔍 Buscar presets"), 200.0));
                if !self.search.is_empty() && ui.small_button("✖").on_hover_text(tr("Clear the search", "Limpar a busca")).clicked() {
                    self.search.clear();
                }
            });
            if self.save_open {
                self.save_form(ui, &palette, current_pedals);
            }
            if let Some((status, error)) = &self.status {
                ui.colored_label(if *error { palette.danger } else { palette.text_dim }, status);
            }
            let query = self.search.trim().to_lowercase();
            let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
            ui.add_space(style::space::M);

            // Setlist.
            if !prefs.setlist.is_empty() {
                style::section_title(ui, "Setlist");
                ui.label(RichText::new(setlist_hint()).small().color(palette.text_dim));
                let mut action: Option<(usize, isize)> = None;
                for (i, entry) in prefs.setlist.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{:>2}.", i + 1)).monospace().color(palette.text_dim));
                        let name = preset_ref_name(entry);
                        let is_current = current == Some(entry);
                        let text = if is_current { RichText::new(&name).strong().color(palette.accent) } else { RichText::new(&name) };
                        if ui.selectable_label(is_current, text).on_hover_text(tr("Load", "Carregar")).clicked()
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
                        if ui.small_button("✖").on_hover_text(tr("Remove from the setlist", "Remover da setlist")).clicked() {
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
            style::section_title(ui, tr("Your presets", "Seus presets"));
            if self.user_presets.is_empty() {
                ui.label(RichText::new(tr(
                    "None saved yet. Build a chain and press 💾 Save current chain above.",
                    "Nenhum salvo ainda. Monte uma cadeia e clique em 💾 Salvar cadeia atual, acima.",
                )).color(palette.text_dim));
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
                        if ui.button("✔").clicked() {
                            let new_name = self.renaming.take().expect("checked above").1;
                            to_rename = Some((listing.path.clone(), new_name));
                        }
                        if ui.button("✖").clicked() {
                            self.renaming = None;
                        }
                    } else if self.confirm_delete.as_ref() == Some(&listing.path) {
                        ui.colored_label(palette.danger, trf!("Delete \"{}\"?", "Excluir \"{}\"?", listing.name));
                        if ui.button(tr("Delete", "Excluir")).clicked() {
                            to_delete = Some(listing.path.clone());
                        }
                        if ui.button(tr("Cancel", "Cancelar")).clicked() {
                            self.confirm_delete = None;
                        }
                    } else {
                        let is_current = current == Some(&preset_ref);
                        let text = if is_current {
                            RichText::new(&listing.name).strong().color(palette.accent)
                        } else {
                            RichText::new(&listing.name)
                        };
                        if ui.selectable_label(is_current, text).on_hover_text(tr("Load", "Carregar")).clicked()
                            && let Ok(p) = preset::load_from_file(&listing.path)
                        {
                            self.save_song = p.song.clone().unwrap_or_default();
                            self.save_artist = p.artist.clone().unwrap_or_default();
                            self.save_name = p.name.clone();
                            loaded = Some(LoadedPreset { pedals: p.pedals, preset_ref, name: p.name });
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🗑").on_hover_text(tr("Delete", "Excluir")).clicked() {
                                self.confirm_delete = Some(listing.path.clone());
                            }
                            if ui.small_button("✏").on_hover_text(tr("Rename", "Renomear")).clicked() {
                                self.renaming = Some((listing.path.clone(), listing.name.clone()));
                            }
                        });
                    }
                });
            }

            if let Some(path) = to_delete {
                if let Err(e) = preset::delete_file(&path) {
                    self.set_status(trf!("Delete failed: {e}", "Falha ao excluir: {e}"), true);
                }
                prefs.setlist.retain(|r| r != &PresetRef::User(path.clone()));
                prefs_changed = true;
                self.events.push(PresetFileEvent::Deleted(path.clone()));
                self.confirm_delete = None;
                self.refresh();
            }

            if let Some((old_path, new_name)) = to_rename {
                if new_name.trim().is_empty() {
                    self.set_status(tr("Enter a name before renaming", "Digite um nome antes de renomear"), true);
                } else if let Ok(mut preset) = preset::load_from_file(&old_path) {
                    preset.name = new_name.trim().to_string();
                    let new_path =
                        old_path.with_file_name(format!("{}.json", preset::sanitize_file_stem(&preset.name)));
                    match preset::save_to_file(&preset, &new_path) {
                        Ok(()) => {
                            self.events.push(PresetFileEvent::Renamed {
                                from: old_path.clone(),
                                to: new_path.clone(),
                                name: preset.name.clone(),
                            });
                            if new_path != old_path {
                                let _ = preset::delete_file(&old_path);
                                for entry in prefs.setlist.iter_mut() {
                                    if *entry == PresetRef::User(old_path.clone()) {
                                        *entry = PresetRef::User(new_path.clone());
                                        prefs_changed = true;
                                    }
                                }
                            }
                            self.set_status(trf!("Renamed to \"{}\"", "Renomeado para \"{}\"", preset.name), false);
                        }
                        Err(e) => self.set_status(trf!("Rename failed: {e}", "Falha ao renomear: {e}"), true),
                    }
                    self.refresh();
                }
            }

            ui.add_space(style::space::M);
            style::section_title(ui, tr("Factory presets", "Presets de fábrica"));
            if prefs.setlist.is_empty() {
                ui.label(RichText::new(setlist_hint()).small().color(palette.text_dim));
            }
            let groups = group_factory_presets(factory_presets().into_iter().filter(|p| matches(&p.name)).collect());
            if groups.is_empty() {
                ui.label(RichText::new(tr("No factory preset matches the search.", "Nenhum preset de fábrica corresponde à busca.")).color(palette.text_dim));
            }
            for (group, presets) in &groups {
                ui.add_space(style::space::XS);
                ui.label(RichText::new(group.title()).strong().color(palette.text_dim));
                style::grid_rows(ui, presets.len(), 220.0, |ui, i, width| {
                    let preset = &presets[i];
                    let preset_ref = PresetRef::Factory(preset.name.clone());
                    let is_current = current == Some(&preset_ref);
                    let (clicked, star_changed) = factory_tile(ui, &palette, prefs, preset, &preset_ref, is_current, width);
                    prefs_changed |= star_changed;
                    if clicked {
                        loaded = Some(LoadedPreset {
                            pedals: preset.pedals.clone(),
                            preset_ref,
                            name: preset.name.clone(),
                        });
                    }
                });
            }
        });

        (loaded, prefs_changed)
    }

    fn close_save_form(&mut self) {
        self.save_open = false;
        self.confirm_overwrite = None;
    }

    /// The "Save current chain" card: name, song and artist. Enter saves,
    /// Esc or Cancel closes it.
    fn save_form(&mut self, ui: &mut Ui, palette: &Palette, current_pedals: &[PedalInstance]) {
        let mut save = false;
        let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        ui.add_space(style::space::XS);
        style::card_frame(palette).inner_margin(egui::Margin::same(style::space::S as i8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let enter = |ui: &Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let tabs_hint = tr(
                "The Tabs page suggests this preset when you open a tab of this song or artist",
                "A página Tabs sugere este preset quando você abre uma tab dessa música ou artista",
            );
            // Name, song, artist: one wrapped row when wide, label-field
            // rows on phones so a label never ends up apart from its field.
            let narrow = ui.ctx().screen_rect().width() < style::NARROW_WIDTH;
            let focus_name = std::mem::take(&mut self.focus_save_name);
            let fields = [
                (tr("Name:", "Nome:"), &mut self.save_name, tr("My crunch", "Meu crunch"), 200.0, None),
                (tr("Song:", "Música:"), &mut self.save_song, tr("optional", "opcional"), 160.0, Some(tabs_hint)),
                (tr("Artist:", "Artista:"), &mut self.save_artist, tr("optional", "opcional"), 160.0, Some(tabs_hint)),
            ];
            let add_fields = |ui: &mut Ui, grid: bool| {
                for (i, (label, text, hint, width, tooltip)) in fields.into_iter().enumerate() {
                    let label = ui.label(label);
                    let width = if grid { ui.available_width() } else { width };
                    let mut field = ui.add(text_field(ui, text, hint, width));
                    if let Some(tooltip) = tooltip {
                        label.on_hover_text(tooltip);
                        field = field.on_hover_text(tooltip);
                    }
                    if i == 0 && focus_name {
                        field.request_focus();
                    }
                    save |= enter(ui, &field);
                    if grid {
                        ui.end_row();
                    }
                }
            };
            if narrow {
                egui::Grid::new("preset_save_fields").num_columns(2).show(ui, |ui| add_fields(ui, true));
            } else {
                ui.horizontal_wrapped(|ui| add_fields(ui, false));
            }
            if self.confirm_overwrite.is_some() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(palette.warn, trf!("\"{}\" already exists.", "\"{}\" já existe.", self.save_name.trim()));
                    if ui.button(tr("Overwrite", "Substituir")).clicked() && self.save(current_pedals, true).is_some() {
                        close = true;
                    }
                    if ui.button(tr("Cancel", "Cancelar")).clicked() {
                        self.confirm_overwrite = None;
                        self.focus_save_name = true;
                    }
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    save |= ui.button(tr("💾 Save", "💾 Salvar")).clicked();
                    close |= ui.button(tr("Cancel", "Cancelar")).clicked();
                });
            }
        });
        if save && self.save(current_pedals, false).is_some() {
            close = true;
        }
        if close {
            self.close_save_form();
        }
    }

    /// "Export current..." button: writes the live chain to a file picked via
    /// a native save dialog (desktop) or a manually entered path (Android,
    /// where `rfd` has no backend).
    #[cfg(not(target_os = "android"))]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalInstance]) {
        if ui.button(tr("Export...", "Exportar..."))
            .on_hover_text(tr("Write the chain to a .json file", "Grava a cadeia em um arquivo .json")).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .set_file_name(format!("{}.json", preset::sanitize_file_stem(&self.save_name)))
                .save_file()
        {
            let preset = self.current_preset(current_pedals);
            match preset::save_to_file(&preset, &path) {
                Ok(()) => self.set_status(trf!("Exported to {}", "Exportado para {}", path.display()), false),
                Err(e) => self.set_status(trf!("Export failed: {e}", "Falha ao exportar: {e}"), true),
            }
        }
    }

    #[cfg(target_os = "android")]
    fn export_button(&mut self, ui: &mut Ui, current_pedals: &[PedalInstance]) {
        ui.label(tr("Export path:", "Caminho:"));
        ui.text_edit_singleline(&mut self.manual_path);
        if ui.button(tr("Export current", "Exportar atual")).clicked() && !self.manual_path.trim().is_empty() {
            let preset = self.current_preset(current_pedals);
            let path = PathBuf::from(self.manual_path.trim());
            match preset::save_to_file(&preset, &path) {
                Ok(()) => self.set_status(trf!("Exported to {}", "Exportado para {}", path.display()), false),
                Err(e) => self.set_status(trf!("Export failed: {e}", "Falha ao exportar: {e}"), true),
            }
        }
    }

    /// "Import..." button: loads a preset from a file picked via a native
    /// open dialog (desktop) or a manually entered path (Android).
    #[cfg(not(target_os = "android"))]
    fn import_button(&mut self, ui: &mut Ui) -> Option<LoadedPreset> {
        if ui.button(tr("Import...", "Importar..."))
            .on_hover_text(tr("Load a preset .json file", "Carrega um arquivo .json de preset")).clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file()
        {
            return self.import_from(path);
        }
        None
    }

    #[cfg(target_os = "android")]
    fn import_button(&mut self, ui: &mut Ui) -> Option<LoadedPreset> {
        if ui.button(tr("Import from path", "Importar do caminho")).clicked() && !self.manual_path.trim().is_empty() {
            let path = PathBuf::from(self.manual_path.trim());
            return self.import_from(path);
        }
        None
    }

    fn import_from(&mut self, path: PathBuf) -> Option<LoadedPreset> {
        match preset::load_from_file(&path) {
            Ok(preset) => {
                self.set_status(trf!("Imported \"{}\"", "\"{}\" importado", preset.name), false);
                Some(LoadedPreset { pedals: preset.pedals, preset_ref: PresetRef::User(path), name: preset.name })
            }
            Err(e) => {
                self.set_status(trf!("Import failed: {e}", "Falha ao importar: {e}"), true);
                None
            }
        }
    }
}

/// A one-line text field as tall as a button (touch-sized on phones).
fn text_field<'t>(ui: &Ui, text: &'t mut String, hint: &str, width: f32) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width)
        .min_size(egui::vec2(0.0, ui.spacing().interact_size.y))
        .vertical_align(egui::Align::Center)
}

/// The line explaining the setlist stars.
fn setlist_hint() -> &'static str {
    tr(
        "☆ adds a preset to the setlist: ◀ ▶, PageUp/PageDown and MIDI walk through it in order.",
        "☆ adiciona um preset à setlist: ◀ ▶, PageUp/PageDown e MIDI percorrem a lista em ordem.",
    )
}

/// Sound families the factory presets are grouped by on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresetGroup {
    Clean,
    Funk,
    Crunch,
    HighGain,
    Ambient,
    /// Factory presets not listed in [`FACTORY_GROUPS`].
    Other,
}

/// Which factory preset (by name, an identifier) goes in which group, in
/// display order.
const FACTORY_GROUPS: &[(PresetGroup, &[&str])] = &[
    (
        PresetGroup::Clean,
        &["Clean Jazz", "Pop Clean", "Country Twang", "Acoustic Simulation", "Surf Rock", "Rockabilly Slapback", "Reggae Skank"],
    ),
    (PresetGroup::Funk, &["Funk Wah", "Funk Envelope"]),
    (PresetGroup::Crunch, &["Blues Crunch", "Classic Rock Rhythm", "Rock Lead"]),
    (PresetGroup::HighGain, &["Metal High Gain", "Metal Djent", "Doom Sludge"]),
    (PresetGroup::Ambient, &["Ambient Pad Looper", "Ambient Ping-Pong", "Shoegaze Wall", "Lo-Fi Tape", "Synth Lead"]),
];

impl PresetGroup {
    fn title(self) -> &'static str {
        match self {
            PresetGroup::Clean => tr("Clean", "Limpo"),
            PresetGroup::Funk => tr("Funk & wah", "Funk e wah"),
            PresetGroup::Crunch => tr("Crunch & rock", "Crunch e rock"),
            PresetGroup::HighGain => tr("High gain", "Alto ganho"),
            PresetGroup::Ambient => tr("Ambient & effects", "Ambiente e efeitos"),
            PresetGroup::Other => tr("Other", "Outros"),
        }
    }
}

/// Splits factory presets into their groups, in [`FACTORY_GROUPS`] order
/// (unknown names last, in "Other", in the order given). Empty groups are
/// left out.
fn group_factory_presets(presets: Vec<Preset>) -> Vec<(PresetGroup, Vec<Preset>)> {
    let position = |name: &str| {
        FACTORY_GROUPS.iter().enumerate().find_map(|(g, (_, names))| names.iter().position(|n| *n == name).map(|i| (g, i)))
    };
    let mut keyed: Vec<((usize, usize), Preset)> = presets
        .into_iter()
        .enumerate()
        .map(|(order, p)| (position(&p.name).unwrap_or((FACTORY_GROUPS.len(), order)), p))
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    let mut groups: Vec<(PresetGroup, Vec<Preset>)> = Vec::new();
    for ((g, _), preset) in keyed {
        let group = FACTORY_GROUPS.get(g).map_or(PresetGroup::Other, |(group, _)| *group);
        match groups.last_mut() {
            Some((last, list)) if *last == group => list.push(preset),
            _ => groups.push((group, vec![preset])),
        }
    }
    groups
}

/// One line naming the pedals of a chain, e.g. "Compressor · Amp · Reverb".
fn chain_summary(pedals: &[PedalInstance]) -> String {
    pedals.iter().map(|p| p.description.name()).collect::<Vec<_>>().join(" · ")
}

/// Shortens `text` to at most `max` characters, ending in "…" when cut.
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// A factory preset tile: setlist star, then the name over a dim summary
/// of the chain; clicking the text loads it. Returns (clicked, whether the
/// star changed the setlist).
fn factory_tile(
    ui: &mut Ui,
    palette: &Palette,
    prefs: &mut UiPrefs,
    preset: &Preset,
    preset_ref: &PresetRef,
    is_current: bool,
    width: f32,
) -> (bool, bool) {
    let frame = egui::Frame::new()
        .fill(palette.card)
        .stroke(egui::Stroke::new(1.0_f32, if is_current { palette.accent } else { palette.card_stroke }))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(6, 4));
    let mut prepared = frame.begin(ui);
    let mut star_changed = false;
    let response = {
        let ui = &mut prepared.content_ui;
        // Inner margin (2 × 6) plus the stroke (2 × 1).
        ui.set_width(width - 14.0);
        ui.horizontal(|ui| {
            star_changed = setlist_star(ui, prefs, preset_ref);
            let text_width = ui.available_width();
            let name_text = if is_current {
                RichText::new(&preset.name).strong().color(palette.accent)
            } else {
                RichText::new(&preset.name).color(palette.text)
            };
            let name = egui::WidgetText::from(name_text).into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                text_width,
                egui::TextStyle::Body,
            );
            let summary = egui::WidgetText::from(RichText::new(chain_summary(&preset.pedals)).small().color(palette.text_dim))
                .into_galley(ui, Some(egui::TextWrapMode::Truncate), text_width, egui::TextStyle::Small);
            let text_height = name.size().y + summary.size().y;
            let height = text_height.max(ui.spacing().interact_size.y);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(text_width, height), egui::Sense::click());
            let top = rect.center().y - text_height / 2.0;
            let name_height = name.size().y;
            ui.painter().galley(egui::pos2(rect.left(), top), name, palette.text);
            ui.painter().galley(egui::pos2(rect.left(), top + name_height), summary, palette.text_dim);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &preset.name));
            response
        })
        .inner
    };
    if response.hovered() && !is_current {
        prepared.frame.stroke = egui::Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.7));
    }
    prepared.end(ui);
    let chain: Vec<&str> = preset.pedals.iter().map(|p| p.description.name()).collect();
    (response.on_hover_text(chain.join(" ➡ ")).clicked(), star_changed)
}

/// ★/☆ toggle adding/removing `preset_ref` from the setlist.
fn setlist_star(ui: &mut Ui, prefs: &mut UiPrefs, preset_ref: &PresetRef) -> bool {
    let palette = Palette::of(ui.ctx());
    let in_setlist = prefs.setlist.contains(preset_ref);
    let (icon, color, hover) = if in_setlist {
        ("★", palette.accent, tr("Remove from setlist", "Tirar da setlist"))
    } else {
        ("☆", palette.text_dim, tr("Add to setlist", "Adicionar à setlist"))
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

    #[test]
    fn every_factory_preset_is_in_a_named_group() {
        let presets = factory_presets();
        let groups = group_factory_presets(presets.clone());
        assert!(groups.iter().all(|(g, _)| *g != PresetGroup::Other), "ungrouped: {groups:?}");
        assert_eq!(groups.iter().map(|(_, list)| list.len()).sum::<usize>(), presets.len());
        let order: Vec<PresetGroup> = groups.iter().map(|(g, _)| *g).collect();
        assert_eq!(
            order,
            [PresetGroup::Clean, PresetGroup::Funk, PresetGroup::Crunch, PresetGroup::HighGain, PresetGroup::Ambient]
        );
        assert_eq!(groups[0].1[0].name, "Clean Jazz");
        // Every listed name is a real factory preset.
        for (_, names) in FACTORY_GROUPS {
            for name in *names {
                assert!(presets.iter().any(|p| p.name == *name), "no factory preset named {name}");
            }
        }
    }

    #[test]
    fn unknown_presets_go_last_and_empty_groups_are_hidden() {
        let named = |name: &str| Preset { name: name.into(), author: None, pedals: Vec::new(), tags: Vec::new(), song: None, artist: None };
        let groups = group_factory_presets(vec![named("Zeta"), named("Rock Lead"), named("Alpha"), named("Clean Jazz")]);
        let summary: Vec<(PresetGroup, Vec<&str>)> =
            groups.iter().map(|(g, list)| (*g, list.iter().map(|p| p.name.as_str()).collect())).collect();
        assert_eq!(
            summary,
            [
                (PresetGroup::Clean, vec!["Clean Jazz"]),
                (PresetGroup::Crunch, vec!["Rock Lead"]),
                (PresetGroup::Other, vec!["Zeta", "Alpha"]),
            ]
        );
        assert!(group_factory_presets(Vec::new()).is_empty());
    }

    #[test]
    fn chain_summary_and_ellipsize() {
        let jazz = load_preset_ref(&PresetRef::Factory("Clean Jazz".into())).expect("factory preset");
        assert_eq!(chain_summary(&jazz.pedals), "Compressor · Amp · Reverb");
        assert_eq!(chain_summary(&[]), "");
        assert_eq!(ellipsize("Short", 10), "Short");
        assert_eq!(ellipsize("A very long preset name", 10), "A very lo…");
        assert_eq!(ellipsize("A very long preset name", 10).chars().count(), 10);
    }

    #[test]
    fn open_save_form_shows_it() {
        let ctx = egui::Context::default();
        let mut panel = PresetPanel::new();
        let mut prefs = UiPrefs::default();
        panel.open_save_form();
        assert!(panel.save_open);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = panel.ui(ui, &[], None, &mut prefs);
            });
        });
        assert!(panel.save_open);
        assert!(!panel.focus_save_name, "focus was requested");
        panel.close_save_form();
        assert!(!panel.save_open);
    }

    /// `setlist_star` only acts on a click, which a headless frame can't produce.
    fn setlist_star_toggled(ui: &mut Ui, prefs: &mut UiPrefs, r: &PresetRef) -> bool {
        setlist_star(ui, prefs, r)
    }
}
