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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub author: Option<String>,
    pub pedals: Vec<PedalDescription>,
    pub tags: Vec<String>,
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
}
