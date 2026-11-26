//! Finding a SoundFont for the tab player's synthesizer, and the card the
//! Tabs page shows while there's none.
//!
//! The search only lists folders (no file is opened), so it's cheap enough
//! to repeat every few seconds on a worker thread: a user who just
//! downloaded a SoundFont gets it picked up without restarting.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Ui};

use crate::frontend::style::{self, Palette};
use crate::shared::i18n::tr;

/// GM banks, best first, matched against file names in lowercase with
/// everything but letters and digits removed ("GeneralUser GS v1.471.sf2"
/// becomes "generalusergsv1471sf2").
const PREFERRED: &[&str] = &["fluidr3gm", "generaluser", "defaultgm", "timgm6mb", "musescoregeneral"];

/// Download page of GeneralUser GS. Its license asks not to link to the
/// download files directly, so the app opens this page instead.
pub const GENERALUSER_URL: &str = "https://www.schristiancollins.com/generaluser.php";

/// Subfolders listed per searched folder, so a huge Downloads folder can't
/// make one search slow.
const MAX_SUBDIRS: usize = 200;

/// rustysynth only reads SoundFont 2: it rejects `.sf3` (compressed samples).
fn is_sf2(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("sf2"))
}

fn is_hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// Index in [`PREFERRED`], or `PREFERRED.len()` for any other bank.
fn name_rank(path: &Path) -> usize {
    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    PREFERRED.iter().position(|p| name.contains(p)).unwrap_or(PREFERRED.len())
}

/// `.sf2` files in `dir` and in its direct subfolders (an unzipped
/// download usually lands in its own folder).
fn sf2_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    let mut subdirs = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        // `is_file` follows symlinks: distributions often install
        // `default-GM.sf2` as a link to the real bank.
        if is_sf2(&path) && path.is_file() {
            found.push(path);
        } else if entry.file_type().is_ok_and(|t| t.is_dir()) && subdirs < MAX_SUBDIRS {
            subdirs += 1;
            let Ok(inner) = std::fs::read_dir(&path) else { continue };
            found.extend(inner.flatten().map(|e| e.path()).filter(|p| !is_hidden(p) && is_sf2(p) && p.is_file()));
        }
    }
    found
}

/// The best SoundFont in `dirs`: a known GM bank first (in [`PREFERRED`]
/// order), else any `.sf2`. Ties go to the earlier folder, then the file
/// name. Paths in `skip` (files that failed to load) are ignored.
pub fn pick_soundfont(dirs: &[PathBuf], skip: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .enumerate()
        .flat_map(|(i, dir)| sf2_files(dir).into_iter().map(move |p| (i, p)))
        .filter(|(_, p)| !skip.contains(p))
        .min_by(|(da, a), (db, b)| name_rank(a).cmp(&name_rank(b)).then(da.cmp(db)).then_with(|| a.cmp(b)))
        .map(|(_, p)| p)
}

/// The app's own SoundFont folder: `<data dir>/guitar_virtual_amp/soundfonts`.
pub fn app_soundfont_dir() -> Option<PathBuf> {
    crate::shared::paths::app_data_dir().map(|d| d.join("soundfonts"))
}

/// Where SoundFonts are usually installed or downloaded on this platform.
fn system_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(target_os = "linux") {
        for dir in ["/usr/share/sounds/sf2", "/usr/share/soundfonts", "/usr/local/share/soundfonts"] {
            dirs.push(PathBuf::from(dir));
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join("Library/Audio/Sounds/Banks"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/share/soundfonts"));
        dirs.push(PathBuf::from("/usr/local/share/soundfonts"));
    }
    if cfg!(target_os = "windows") {
        dirs.push(PathBuf::from(r"C:\soundfonts"));
    }
    dirs.extend(dirs::download_dir());
    dirs
}

/// Looks for a SoundFont: one the user put in the app's folder wins, else
/// the best of the usual places. Lists folders only; still, call it from a
/// worker thread.
pub fn find_soundfont(skip: &[PathBuf]) -> Option<PathBuf> {
    let own: Vec<PathBuf> = app_soundfont_dir().into_iter().collect();
    pick_soundfont(&own, skip).or_else(|| pick_soundfont(&system_dirs(), skip))
}

/// What the user did on the no-SoundFont card.
#[derive(Debug, PartialEq)]
pub enum CardAction {
    None,
    /// Picked this file in the file dialog.
    Pick(PathBuf),
    /// Android: use the path typed in the page's path field.
    #[cfg(target_os = "android")]
    UsePathField,
}

/// The card explaining why playback is silent and how to fix it.
/// `searching` shows a spinner while a search runs.
pub fn missing_soundfont_card(ui: &mut Ui, searching: bool) -> CardAction {
    let palette = Palette::of(ui.ctx());
    let mut action = CardAction::None;
    style::card_frame(&palette).stroke(egui::Stroke::new(1.0_f32, palette.warn)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("🔇").color(palette.warn));
            ui.label(
                RichText::new(tr(
                    "Tab playback needs a SoundFont (.sf2), a bank of instrument sounds.",
                    "Para tocar a tab é preciso um SoundFont (.sf2), um banco de sons de instrumentos.",
                ))
                .strong()
                .color(palette.text),
            );
        });
        ui.add_space(style::space::XS);
        ui.horizontal_wrapped(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("📂 Choose SoundFont...", "📂 Escolher SoundFont...")).clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("SoundFont", &["sf2"]).pick_file()
            {
                action = CardAction::Pick(path);
            }
            #[cfg(target_os = "android")]
            if ui.button(tr("Use path as SoundFont", "Usar caminho como SoundFont")).clicked() {
                action = CardAction::UsePathField;
            }
            if ui
                .button(tr("➡ Get GeneralUser GS (free, 31 MB)", "➡ Obter GeneralUser GS (grátis, 31 MB)"))
                .on_hover_text(GENERALUSER_URL)
                .clicked()
                && let Err(e) = webbrowser::open(GENERALUSER_URL)
            {
                log::warn!("could not open {GENERALUSER_URL}: {e}");
            }
            if searching {
                ui.spinner();
            }
        });
        ui.add_space(style::space::XS);
        ui.label(
            RichText::new(tr(
                "A .sf2 saved (or unzipped) in your Downloads folder is picked up automatically.",
                "Um .sf2 salvo (ou descompactado) na pasta Downloads é encontrado automaticamente.",
            ))
            .small()
            .color(palette.text_dim),
        );
        if cfg!(target_os = "linux") {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Or install one:", "Ou instale um:")).small().color(palette.text_dim));
                ui.label(RichText::new("sudo apt install fluid-soundfont-gm").monospace().small());
            });
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gva_sf_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn prefers_known_gm_banks_then_any_sf2() {
        let root = temp_dir("pick");
        let (a, b) = (root.join("a"), root.join("b"));
        touch(&a.join("Piano.sf2"));
        touch(&a.join("MuseScore_General.sf3"));
        touch(&b.join("aaa.sf2"));
        let dirs = vec![a.clone(), b.clone(), root.join("missing")];
        // No known bank: any .sf2, earlier folder first. .sf3 is skipped.
        assert_eq!(pick_soundfont(&dirs, &[]), Some(a.join("Piano.sf2")));
        // A known bank in a later folder (in a subfolder, as unzipped) beats it.
        touch(&b.join("GeneralUser GS 1.471/GeneralUser GS v1.471.sf2"));
        assert_eq!(pick_soundfont(&dirs, &[]), Some(b.join("GeneralUser GS 1.471/GeneralUser GS v1.471.sf2")));
        touch(&b.join("TimGM6mb.sf2"));
        touch(&a.join("fluidr3_gm.SF2"));
        assert_eq!(pick_soundfont(&dirs, &[]), Some(a.join("fluidr3_gm.SF2")));
        // Files that failed to load are skipped.
        let skip = vec![a.join("fluidr3_gm.SF2")];
        assert_eq!(pick_soundfont(&dirs, &skip), Some(b.join("GeneralUser GS 1.471/GeneralUser GS v1.471.sf2")));
        // Hidden files and folders don't count.
        let c = root.join("c");
        touch(&c.join(".hidden.sf2"));
        touch(&c.join(".cache/x.sf2"));
        assert_eq!(pick_soundfont(&[c], &[]), None);
        assert_eq!(pick_soundfont(&[], &[]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ranks_names_ignoring_case_and_punctuation() {
        assert_eq!(name_rank(Path::new("/x/FluidR3_GM.sf2")), 0);
        assert_eq!(name_rank(Path::new("/x/GeneralUser-GS.sf2")), 1);
        assert_eq!(name_rank(Path::new("/x/default-GM.sf2")), 2);
        assert_eq!(name_rank(Path::new("/x/TimGM6mb.sf2")), 3);
        assert_eq!(name_rank(Path::new("/x/MuseScore_General.sf2")), 4);
        assert_eq!(name_rank(Path::new("/x/Strings.sf2")), PREFERRED.len());
    }
}
