use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::pedals::PedalDescription;

/// A named, shareable snapshot of a pedal chain.
///
/// Presets are the on-disk unit for saving/loading a pedal chain configuration.
/// They're serialized as human-diffable JSON so factory preset packs can be
/// hand-authored/reviewed as plain files.
///
/// `song`/`artist` are optional free-form metadata (e.g. `song: Some("Sweet
/// Child O' Mine")`, `artist: Some("inspired by Guns N' Roses")`) describing
/// what a preset is going for tonally. They're not used by any loading logic
/// today, but are the seam a future preset-sharing/social feature (searching,
/// browsing by artist, etc.) will build on -- kept `#[serde(default)]` so
/// presets saved before this field existed still deserialize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub author: Option<String>,
    pub pedals: Vec<PedalDescription>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub song: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
}

/// Errors that can occur while saving or loading a [`Preset`].
#[derive(Debug)]
pub enum PresetError {
    Io(std::io::Error),
    Serde(serde_json::Error),
}

impl fmt::Display for PresetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PresetError::Io(e) => write!(f, "preset I/O error: {e}"),
            PresetError::Serde(e) => write!(f, "preset (de)serialization error: {e}"),
        }
    }
}

impl std::error::Error for PresetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PresetError::Io(e) => Some(e),
            PresetError::Serde(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for PresetError {
    fn from(e: std::io::Error) -> Self {
        PresetError::Io(e)
    }
}

impl From<serde_json::Error> for PresetError {
    fn from(e: serde_json::Error) -> Self {
        PresetError::Serde(e)
    }
}

/// Serializes `preset` to pretty-printed JSON and writes it to `path`,
/// creating any missing parent directories first.
pub fn save_to_file(preset: &Preset, path: &Path) -> Result<(), PresetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(preset)?;
    fs::write(path, json)?;
    Ok(())
}

/// Reads and deserializes a [`Preset`] from `path`.
pub fn load_from_file(path: &Path) -> Result<Preset, PresetError> {
    let contents = fs::read_to_string(path)?;
    let preset = serde_json::from_str(&contents)?;
    Ok(preset)
}

/// Fixed subpath (relative to a platform data directory) where user presets live.
const PRESET_SUBPATH: &str = "guitar_virtual_amp/presets";

/// Appends the fixed preset subpath onto an arbitrary base directory.
///
/// This is the piece callers on platforms without a resolvable data
/// directory (e.g. Android, which has no `$HOME`) can use directly, by
/// supplying their own base path.
pub fn user_preset_dir_in(base: &Path) -> PathBuf {
    base.join(PRESET_SUBPATH)
}

/// Resolves the per-platform user preset directory on desktop (Linux/macOS/Windows)
/// using `dirs::data_dir()`. Returns `None` if no data directory can be resolved
/// for the current platform (e.g. Android) — callers on those platforms should
/// fall back to [`user_preset_dir_in`] with an explicit base path.
pub fn user_preset_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|base| user_preset_dir_in(&base))
}

/// One entry in a preset listing: the preset's own `name` field plus the file
/// it was loaded from (needed for subsequent load/rename/delete calls).
#[derive(Clone, Debug)]
pub struct PresetListing {
    pub name: String,
    pub path: PathBuf,
}

/// Lists every `.json` preset file directly inside `dir` (non-recursive),
/// reading just enough of each file to recover its `name` field. Files that
/// fail to parse as a [`Preset`] are silently skipped rather than failing the
/// whole listing -- one corrupt/foreign file shouldn't hide the rest. Returns
/// an empty list if `dir` doesn't exist.
pub fn list_presets(dir: &Path) -> Vec<PresetListing> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut listings: Vec<PresetListing> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let preset = load_from_file(&path).ok()?;
            Some(PresetListing {
                name: preset.name,
                path,
            })
        })
        .collect();

    listings.sort_by(|a, b| a.name.cmp(&b.name));
    listings
}

/// Deletes the preset file at `path`.
pub fn delete_file(path: &Path) -> Result<(), PresetError> {
    fs::remove_file(path)?;
    Ok(())
}

/// Sanitizes an arbitrary preset name into a filesystem-safe file stem by
/// replacing anything that isn't alphanumeric, `-`, or `_` with `_`. Used to
/// derive a preset's filename from its user-entered `name` field.
pub fn sanitize_file_stem(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sanitized.is_empty() {
        "preset".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::pedals::{AmpParams, DelayParams};

    fn sample_preset() -> Preset {
        Preset {
            name: "Test Preset".to_string(),
            author: Some("Breno".to_string()),
            pedals: vec![
                PedalDescription::Amp(AmpParams {
                    gain: 1.5,
                    ..Default::default()
                }),
                PedalDescription::Delay(DelayParams::default()),
            ],
            tags: vec!["clean".to_string(), "test".to_string()],
            song: Some("Test Song".to_string()),
            artist: Some("Test Artist".to_string()),
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "guitar_virtual_amp_preset_test_{}_{}",
            std::process::id(),
            name
        ));
        dir
    }

    #[test]
    fn preset_round_trips_through_file() {
        let preset = sample_preset();
        let path = unique_temp_path("round_trip").join("preset.json");

        save_to_file(&preset, &path).expect("save preset");
        let loaded = load_from_file(&path).expect("load preset");

        assert_eq!(preset, loaded);

        // Clean up.
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_creates_missing_directories() {
        let preset = sample_preset();
        let base = unique_temp_path("missing_dir");
        let path = base.join("nested").join("deeper").join("preset.json");

        assert!(!path.parent().unwrap().exists());

        save_to_file(&preset, &path).expect("save preset into missing dir");

        assert!(path.exists());
        let loaded = load_from_file(&path).expect("load preset");
        assert_eq!(preset, loaded);

        // Clean up.
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn user_preset_dir_in_appends_fixed_subpath() {
        let base = Path::new("/some/base");
        let dir = user_preset_dir_in(base);
        assert_eq!(dir, base.join("guitar_virtual_amp/presets"));
    }

    #[test]
    fn presets_saved_before_song_artist_existed_still_deserialize() {
        // No "song"/"artist" keys at all -- simulates a preset file written
        // before those fields were added to the schema.
        let json = r#"{
            "name": "Old Preset",
            "author": null,
            "pedals": [],
            "tags": []
        }"#;

        let preset: Preset = serde_json::from_str(json).expect("deserialize legacy preset");
        assert_eq!(preset.song, None);
        assert_eq!(preset.artist, None);
    }

    #[test]
    fn list_presets_finds_json_files_and_reads_their_names() {
        let dir = unique_temp_path("list_presets");
        let mut preset_a = sample_preset();
        preset_a.name = "Preset A".to_string();
        let mut preset_b = sample_preset();
        preset_b.name = "Preset B".to_string();

        save_to_file(&preset_a, &dir.join("a.json")).expect("save a");
        save_to_file(&preset_b, &dir.join("b.json")).expect("save b");
        fs::write(dir.join("not_a_preset.txt"), "ignore me").expect("write junk file");

        let listings = list_presets(&dir);
        let names: Vec<&str> = listings.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["Preset A", "Preset B"]);

        // Clean up.
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_presets_on_missing_dir_returns_empty() {
        let dir = unique_temp_path("does_not_exist");
        assert!(list_presets(&dir).is_empty());
    }

    #[test]
    fn delete_file_removes_the_preset() {
        let preset = sample_preset();
        let path = unique_temp_path("delete").join("preset.json");
        save_to_file(&preset, &path).expect("save preset");
        assert!(path.exists());

        delete_file(&path).expect("delete preset");
        assert!(!path.exists());

        // Clean up.
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sanitize_file_stem_replaces_unsafe_characters() {
        assert_eq!(sanitize_file_stem("Metal High-Gain_v2"), "Metal_High-Gain_v2");
        assert_eq!(sanitize_file_stem("Ambient / Reverb"), "Ambient___Reverb");
        assert_eq!(sanitize_file_stem(""), "preset");
    }
}
