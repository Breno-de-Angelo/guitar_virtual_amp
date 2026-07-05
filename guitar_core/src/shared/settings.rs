//! Small persisted-across-restarts app config (Phase 3.4 of ROADMAP.md).
//!
//! Window size/position is handled separately by eframe's own "persistence"
//! feature (see `guitar_desktop::main`), so this only covers the state that's
//! specific to this app: last-used input/output device and the last-loaded
//! preset, so a restart drops the user back roughly where they left off.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identifies which preset was last loaded, so it can be reloaded on the next
/// launch. User presets are referenced by their file path (works even if
/// renamed by editing the file, though a rename via the UI changes the path);
/// factory presets have no file on disk, so they're referenced by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PresetRef {
    User(PathBuf),
    Factory(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub last_preset: Option<PresetRef>,
    /// Requested host-callback buffer size in frames (see
    /// `backend::capture::resolve_stream_config`); `None` uses the
    /// `shared::config::GLOBAL_CONFIG` default.
    pub buffer_frames: Option<u32>,
}

const SETTINGS_SUBPATH: &str = "guitar_virtual_amp/settings.json";

/// Resolves the per-platform settings file path via `dirs::config_dir()`.
/// Returns `None` on platforms without a resolvable config directory (e.g.
/// Android), same caveat as `preset::user_preset_dir`.
pub fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join(SETTINGS_SUBPATH))
}

/// Loads settings from disk, falling back to `AppSettings::default()` if the
/// file doesn't exist yet or fails to parse -- a missing/corrupt settings
/// file should never block the app from starting.
pub fn load_settings() -> AppSettings {
    settings_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Best-effort save: failures (no writable config dir, I/O error) are
/// silently ignored since losing "remember last device" state isn't worth
/// surfacing an error to the user over.
pub fn save_settings(settings: &AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let settings = AppSettings {
            input_device: Some("Mic".to_string()),
            output_device: Some("Speakers".to_string()),
            last_preset: Some(PresetRef::Factory("Clean Jazz".to_string())),
            buffer_frames: Some(256),
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let restored: AppSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, restored);
    }

    #[test]
    fn missing_file_yields_default() {
        // load_settings must never panic even if no settings file exists yet;
        // exercised indirectly since we can't isolate `dirs::config_dir()`
        // per-test, so just confirm the fallback path itself doesn't panic.
        let _ = load_settings();
    }
}
