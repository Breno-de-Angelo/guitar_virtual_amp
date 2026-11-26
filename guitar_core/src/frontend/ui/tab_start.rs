//! What the Tabs page shows while no tab is open: a drop zone, the recently
//! opened tabs, the tabs the app transcribed and the user's tab folder.
//!
//! Listing reads folders and sidecars, so it runs on worker threads and the
//! results are cached; the cache refreshes when the page is opened again,
//! when the tab folder changes, or on ⟳.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Ui};

use crate::frontend::style::{self, Palette};
use crate::frontend::ui::tab_player::{poll, spawn, Pending};
use crate::frontend::ui::transcribe::repository_dir;
use crate::shared::i18n::tr;
use crate::shared::settings::{save_settings, AppSettings};
use crate::shared::tab::import::SUPPORTED_EXTENSIONS;
use crate::shared::tab::library::{scan_library, words};
use crate::shared::tab::repository;
use crate::trf;

/// Length of `AppSettings::recent_tabs`.
pub const MAX_RECENT: usize = 10;
/// Transcribed tabs shown before "Show all".
const TRANSCRIBED_SHOWN: usize = 10;
/// Library rows shown for a search.
const LIBRARY_SHOWN: usize = 20;
/// Narrowest column of the lists; narrower windows stack them.
const COLUMN_WIDTH: f32 = 300.0;

/// Moves `path` to the front of the recent list (adding it if new) and
/// keeps at most [`MAX_RECENT`] entries.
pub fn push_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(MAX_RECENT);
}

/// A tab file in the library with its name folded for searching.
struct LibraryEntry {
    path: PathBuf,
    /// Lowercase, accent-free words of the file name, space-separated.
    folded: String,
}

fn fold_name(path: &Path) -> String {
    words(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).join(" ")
}

/// Indices of the entries whose name contains every word of `query` (case
/// and accents ignored), at most `max`, and how many matched in total.
fn filter_by_name(names: &[&str], query: &str, max: usize) -> (Vec<usize>, usize) {
    let wanted = words(query);
    let mut shown = Vec::new();
    let mut total = 0;
    for (i, name) in names.iter().enumerate() {
        if wanted.iter().all(|w| name.contains(w.as_str())) {
            total += 1;
            if shown.len() < max {
                shown.push(i);
            }
        }
    }
    (shown, total)
}

struct TranscribedTab {
    path: PathBuf,
    title: String,
    artist: String,
}

/// Recent tabs that still exist and the repository's tabs.
struct LocalListing {
    recent: Vec<PathBuf>,
    transcribed: Vec<TranscribedTab>,
}

fn list_local(recent: Vec<PathBuf>, repo: Option<PathBuf>) -> Result<LocalListing, String> {
    let recent = recent.into_iter().filter(|p| p.is_file()).collect();
    let transcribed = repo
        .map(|dir| repository::list_tabs(&dir))
        .unwrap_or_default()
        .into_iter()
        .map(|(path, meta)| {
            let title = if meta.title.trim().is_empty() {
                path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                meta.title.trim().to_string()
            };
            TranscribedTab { path, title, artist: meta.artist.trim().to_string() }
        })
        .collect();
    Ok(LocalListing { recent, transcribed })
}

fn list_library(dir: PathBuf) -> Result<(PathBuf, Vec<LibraryEntry>), String> {
    let entries = scan_library(&dir)
        .into_iter()
        .map(|path| LibraryEntry { folded: fold_name(&path), path })
        .collect();
    Ok((dir, entries))
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// A clickable two-line row: `title`, and `detail` dimmed under it.
fn tab_link(ui: &mut Ui, title: &str, detail: &str) -> bool {
    let palette = Palette::of(ui.ctx());
    let background = ui.painter().add(egui::Shape::Noop);
    let narrow = ui.ctx().screen_rect().width() < style::NARROW_WIDTH;
    let response = ui
        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            ui.set_width(ui.available_width());
            if narrow {
                ui.set_min_height(style::TOUCH_TARGET);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(egui::Label::new(RichText::new(title).color(palette.text)).truncate().selectable(false));
            if !detail.is_empty() {
                ui.add(egui::Label::new(RichText::new(detail).small().color(palette.text_dim)).truncate().selectable(false));
            }
        })
        .response
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        let rect = response.rect.expand2(egui::vec2(style::space::XS, 2.0));
        ui.painter().set(background, egui::Shape::rect_filled(rect, 4.0, palette.accent.gamma_multiply(0.15)));
    }
    response.clicked()
}

fn empty_note(ui: &mut Ui, text: &str) {
    let palette = Palette::of(ui.ctx());
    ui.label(RichText::new(text).color(palette.text_dim));
}

/// The no-tab-open page below the Open/Transcribe buttons.
#[derive(Default)]
pub struct TabStartPage {
    recent: Vec<PathBuf>,
    transcribed: Vec<TranscribedTab>,
    loading_local: Pending<LocalListing>,
    /// Folder the cached library listing is for.
    library_dir: Option<PathBuf>,
    library: Vec<LibraryEntry>,
    loading_library: Pending<(PathBuf, Vec<LibraryEntry>)>,
    show_all_transcribed: bool,
    query: String,
    /// `(query, shown indices, total matches)` for the cached library.
    filtered: Option<(String, Vec<usize>, usize)>,
    /// Frame this page was last drawn in, to notice it being opened again.
    last_frame: Option<u64>,
}

impl TabStartPage {
    fn refresh(&mut self, settings: &AppSettings) {
        let (recent, repo) = (settings.recent_tabs.clone(), repository_dir(settings));
        self.loading_local = Some(spawn(move || list_local(recent, repo)));
        self.refresh_library(settings);
    }

    fn refresh_library(&mut self, settings: &AppSettings) {
        self.library_dir = settings.tab_library_dir.clone();
        self.filtered = None;
        match self.library_dir.clone() {
            Some(dir) => self.loading_library = Some(spawn(move || list_library(dir))),
            None => {
                self.library.clear();
                self.loading_library = None;
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context, settings: &AppSettings) {
        let frame = ctx.cumulative_frame_nr();
        let reopened = self.last_frame.is_none_or(|last| frame > last + 1);
        self.last_frame = Some(frame);
        if reopened {
            self.refresh(settings);
        } else if settings.tab_library_dir != self.library_dir {
            self.refresh_library(settings);
        }
        if let Some(Ok(listing)) = poll(&mut self.loading_local) {
            self.recent = listing.recent;
            self.transcribed = listing.transcribed;
        }
        if let Some(Ok((dir, entries))) = poll(&mut self.loading_library)
            && Some(&dir) == self.library_dir.as_ref()
        {
            self.library = entries;
            self.filtered = None;
        }
        if self.loading_local.is_some() || self.loading_library.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Draws the page. Returns a tab to open.
    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) -> Option<PathBuf> {
        self.poll(ui.ctx(), settings);
        let palette = Palette::of(ui.ctx());
        drop_zone(ui, &palette);
        ui.add_space(style::space::M);
        let mut open = None;
        style::grid_rows(ui, 3, COLUMN_WIDTH, |ui, i, _| {
            ui.vertical(|ui| {
                style::card_frame(&palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let picked = match i {
                        0 => self.recent_card(ui),
                        1 => self.transcribed_card(ui),
                        _ => self.library_card(ui, settings),
                    };
                    open = open.take().or(picked);
                });
            });
        });
        open
    }

    fn recent_card(&mut self, ui: &mut Ui) -> Option<PathBuf> {
        style::section_title(ui, tr("Recent", "Recentes"));
        if self.recent.is_empty() {
            empty_note(ui, tr("Tabs you open show up here.", "As tabs que você abrir aparecem aqui."));
        }
        let mut open = None;
        for path in &self.recent {
            let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
            if tab_link(ui, &file_name(path), &folder) {
                open = Some(path.clone());
            }
        }
        open
    }

    fn transcribed_card(&mut self, ui: &mut Ui) -> Option<PathBuf> {
        ui.horizontal(|ui| {
            style::section_title(ui, tr("Transcribed by the app", "Transcritas pelo app"));
            if self.loading_local.is_some() {
                ui.spinner();
            }
        });
        if self.transcribed.is_empty() {
            empty_note(
                ui,
                tr(
                    "Tabs made with \"Transcribe audio\" are saved here.",
                    "As tabs feitas com \"Transcrever áudio\" ficam aqui.",
                ),
            );
        }
        let shown = if self.show_all_transcribed { self.transcribed.len() } else { TRANSCRIBED_SHOWN };
        let mut open = None;
        for tab in self.transcribed.iter().take(shown) {
            if tab_link(ui, &tab.title, &tab.artist) {
                open = Some(tab.path.clone());
            }
        }
        if self.transcribed.len() > TRANSCRIBED_SHOWN {
            let label = if self.show_all_transcribed {
                tr("Show fewer", "Mostrar menos").to_string()
            } else {
                trf!("Show all ({})", "Mostrar todas ({})", self.transcribed.len())
            };
            if ui.button(label).clicked() {
                self.show_all_transcribed = !self.show_all_transcribed;
            }
        }
        open
    }

    fn library_card(&mut self, ui: &mut Ui, settings: &mut AppSettings) -> Option<PathBuf> {
        let palette = Palette::of(ui.ctx());
        let mut choose = false;
        let mut rescan = false;
        ui.horizontal_wrapped(|ui| {
            style::section_title(ui, tr("Library folder", "Pasta de tabs"));
            if self.loading_library.is_some() {
                ui.spinner();
            } else if self.library_dir.is_some() {
                ui.label(RichText::new(trf!("{} tabs", "{} tabs", self.library.len())).color(palette.text_dim));
                rescan = ui.small_button("⟳").on_hover_text(tr("Scan the folder again", "Ler a pasta de novo")).clicked();
            }
        });
        let Some(dir) = self.library_dir.clone() else {
            empty_note(
                ui,
                tr(
                    "Choose the folder where you keep your Guitar Pro files to list them here (the Spotify page uses it too).",
                    "Escolha a pasta onde você guarda seus arquivos Guitar Pro para listá-los aqui (a página do Spotify também a usa).",
                ),
            );
            #[cfg(not(target_os = "android"))]
            {
                choose = ui.button(tr("📂 Choose folder...", "📂 Escolher pasta...")).clicked();
            }
            self.choose_folder(choose, settings);
            return None;
        };
        ui.horizontal(|ui| {
            #[cfg(not(target_os = "android"))]
            {
                choose = ui.small_button("📂").on_hover_text(tr("Choose another folder", "Escolher outra pasta")).clicked();
            }
            ui.add(egui::Label::new(RichText::new(dir.display().to_string()).small().color(palette.text_dim)).truncate());
        });
        if rescan {
            self.refresh_library(settings);
        }
        self.choose_folder(choose, settings);
        if self.library.is_empty() {
            if self.loading_library.is_none() {
                empty_note(ui, tr("No Guitar Pro files in this folder.", "Nenhum arquivo Guitar Pro nesta pasta."));
            }
            return None;
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text(tr("🔎 Search by file name", "🔎 Buscar pelo nome do arquivo"))
                .desired_width(f32::INFINITY),
        );
        if self.filtered.as_ref().is_none_or(|(q, _, _)| *q != self.query) {
            let names: Vec<&str> = self.library.iter().map(|e| e.folded.as_str()).collect();
            let (shown, total) = filter_by_name(&names, &self.query, LIBRARY_SHOWN);
            self.filtered = Some((self.query.clone(), shown, total));
        }
        let (shown, total) = self.filtered.as_ref().map(|(_, s, t)| (s.clone(), *t)).unwrap_or_default();
        if total == 0 {
            empty_note(ui, tr("No file matches.", "Nenhum arquivo corresponde."));
        }
        let mut open = None;
        for i in shown.iter().copied() {
            let Some(entry) = self.library.get(i) else { continue };
            let folder = entry
                .path
                .parent()
                .and_then(|p| p.strip_prefix(&dir).ok())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if tab_link(ui, &file_name(&entry.path), &folder) {
                open = Some(entry.path.clone());
            }
        }
        if total > shown.len() {
            ui.label(
                RichText::new(trf!(
                    "{} more: type to narrow the list",
                    "mais {}: digite para filtrar",
                    total - shown.len()
                ))
                .small()
                .color(palette.text_dim),
            );
        }
        open
    }

    /// Opens the folder dialog when `choose` and saves the pick.
    fn choose_folder(&mut self, choose: bool, settings: &mut AppSettings) {
        #[cfg(not(target_os = "android"))]
        if choose && let Some(dir) = rfd::FileDialog::new().pick_folder() {
            settings.tab_library_dir = Some(dir);
            save_settings(settings);
            self.refresh_library(settings);
        }
        #[cfg(target_os = "android")]
        let _ = (choose, settings);
    }
}

/// A framed area inviting to drop a file, highlighted while one is dragged
/// over the window.
fn drop_zone(ui: &mut Ui, palette: &Palette) {
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let stroke = if hovering { palette.accent } else { palette.card_stroke };
    egui::Frame::new()
        .stroke(egui::Stroke::new(if hovering { 2.0_f32 } else { 1.0_f32 }, stroke))
        .fill(if hovering { palette.accent.gamma_multiply(0.08) } else { egui::Color32::TRANSPARENT })
        .corner_radius(egui::CornerRadius::same(style::CARD_RADIUS))
        .inner_margin(egui::Margin::same(style::space::XL as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(tr("⬇ Drop a Guitar Pro file here", "⬇ Arraste um arquivo Guitar Pro para cá"))
                        .size(style::text::HEADING)
                        .color(if hovering { palette.accent } else { palette.text }),
                );
                let extensions: Vec<String> = SUPPORTED_EXTENSIONS.iter().map(|e| format!(".{e}")).collect();
                ui.label(
                    RichText::new(trf!(
                        "{} · or a .sf2 SoundFont for the synth",
                        "{} · ou um SoundFont .sf2 para o sintetizador",
                        extensions.join(", ")
                    ))
                    .color(palette.text_dim),
                );
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_list_dedups_orders_and_caps() {
        let mut list = Vec::new();
        for i in 0..12 {
            push_recent(&mut list, Path::new(&format!("/t/{i}.gp")));
        }
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], PathBuf::from("/t/11.gp"), "most recent first");
        assert_eq!(list[MAX_RECENT - 1], PathBuf::from("/t/2.gp"), "oldest dropped");
        // Reopening moves it to the front without a duplicate.
        push_recent(&mut list, Path::new("/t/5.gp"));
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], PathBuf::from("/t/5.gp"));
        assert_eq!(list.iter().filter(|p| p.as_path() == Path::new("/t/5.gp")).count(), 1);
        assert_eq!(list[1], PathBuf::from("/t/11.gp"));
    }

    #[test]
    fn library_filter_ignores_case_and_accents_and_caps() {
        let folded: Vec<String> = ["AC_DC - Back in Black.gp5", "Legião Urbana - Tempo Perdido.gp", "Black Sabbath - Paranoid.gp3"]
            .iter()
            .map(|n| fold_name(Path::new(n)))
            .collect();
        let names: Vec<&str> = folded.iter().map(String::as_str).collect();
        assert_eq!(filter_by_name(&names, "", 10), (vec![0, 1, 2], 3));
        assert_eq!(filter_by_name(&names, "BLACK", 10), (vec![0, 2], 2));
        assert_eq!(filter_by_name(&names, "legiao perd", 10), (vec![1], 1));
        assert_eq!(filter_by_name(&names, "black", 1), (vec![0], 2), "capped, total still counted");
        assert_eq!(filter_by_name(&names, "metallica", 10), (vec![], 0));
    }

    #[test]
    fn local_listing_skips_missing_recent_files() {
        let dir = std::env::temp_dir().join(format!("gva_tab_start_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let there = dir.join("there.gp5");
        std::fs::write(&there, b"x").unwrap();
        let listing = list_local(vec![dir.join("gone.gp5"), there.clone()], Some(dir.join("repo"))).unwrap();
        assert_eq!(listing.recent, vec![there]);
        assert!(listing.transcribed.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
