//! Small persisted-across-restarts app config (Phase 3.4 of ROADMAP.md).
//!
//! Window size/position is handled separately by eframe's own "persistence"
//! feature (see `guitar_desktop::main`), so this only covers the state that's
//! specific to this app: last-used input/output device and the last-loaded
//! preset, so a restart drops the user back roughly where they left off.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::transcribe::fingering_net::Fingering;
use crate::transcribe::guitar_crnn::GuitarModel;

/// Identifies which preset was last loaded, so it can be reloaded on the next
/// launch. User presets are referenced by their file path (works even if
/// renamed by editing the file, though a rename via the UI changes the path);
/// factory presets have no file on disk, so they're referenced by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PresetRef {
    User(PathBuf),
    Factory(String),
}

/// Per-tab backing-track choice and manual sync, so reopening a tab brings
/// its audio back already aligned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabSyncSettings {
    pub audio_path: Option<PathBuf>,
    /// Backing-track time (seconds) at which bar 1 starts.
    pub offset_secs: f64,
    /// Recording tempo divided by the tab's notated tempo.
    pub tempo_ratio: f64,
}

impl Default for TabSyncSettings {
    fn default() -> Self {
        Self { audio_path: None, offset_secs: 0.0, tempo_ratio: 1.0 }
    }
}

/// Tab-player practice mode knobs (see `frontend::lib::practice`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PracticePrefs {
    /// Added to the measured audio round-trip latency, for interfaces whose
    /// real latency is higher than what the driver reports.
    pub extra_latency_ms: f32,
    /// How early/late a note may be picked and still count.
    pub tolerance_ms: f32,
    /// 0 = lenient, 1 = strict.
    pub strictness: f32,
}

impl Default for PracticePrefs {
    fn default() -> Self {
        Self { extra_latency_ms: 0.0, tolerance_ms: 120.0, strictness: 0.5 }
    }
}

// `serde(default)` keeps settings files written by older versions loadable
// when fields are added (a parse failure would reset every setting).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub last_preset: Option<PresetRef>,
    /// Requested host-callback buffer size in frames (see
    /// `backend::capture::resolve_stream_config`); `None` uses the
    /// `shared::config::GLOBAL_CONFIG` default.
    pub buffer_frames: Option<u32>,
    /// SoundFont (.sf2) used by the tab player's synthesizer.
    pub soundfont_path: Option<PathBuf>,
    /// Last tab opened in the tab player, reopened on launch.
    pub last_tab: Option<PathBuf>,
    /// Keyed by the tab file's path.
    pub tab_sync: BTreeMap<String, TabSyncSettings>,
    pub practice: PracticePrefs,
    /// Folder of Guitar Pro files matched against Spotify tracks by name.
    pub tab_library_dir: Option<PathBuf>,
    /// Tabs the user linked by hand, keyed by Spotify track ID.
    pub spotify_tab_links: BTreeMap<String, PathBuf>,
    /// Where transcriptions are saved; `None` uses
    /// `shared::tab::repository::default_repository_dir`.
    pub tab_repository_dir: Option<PathBuf>,
    /// Guitar CRNN for transcriptions when the electric fine-tune is
    /// installed (see `transcribe::guitar_crnn::GuitarModel`).
    pub transcription_guitar_model: GuitarModel,
    /// Learned fingering when its model is installed, or the Viterbi
    /// (see `transcribe::fingering_net::Fingering`).
    pub transcription_fingering: Fingering,
    /// Also estimate each guitar track's tone and save it as a preset
    /// (`tone::save_preset`). Off by default: experimental.
    pub transcription_tone: bool,
}

const SETTINGS_SUBPATH: &str = "guitar_virtual_amp/settings.json";

/// Resolves the per-platform settings file path via `dirs::config_dir()`.
/// Returns `None` on platforms without a resolvable config directory (e.g.
/// Android), same caveat as `preset::user_preset_dir`.
pub fn settings_path() -> Option<PathBuf> {
    // Unit tests drive UI panels that save settings; never let them touch
    // the developer's real settings file.
    if cfg!(test) {
        return None;
    }
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
            soundfont_path: Some(PathBuf::from("/sf/FluidR3_GM.sf2")),
            last_tab: Some(PathBuf::from("/tabs/song.gp5")),
            tab_sync: BTreeMap::from([(
                "/tabs/song.gp5".to_string(),
                TabSyncSettings {
                    audio_path: Some(PathBuf::from("/music/song.mp3")),
                    offset_secs: 1.25,
                    tempo_ratio: 1.01,
                },
            )]),
            practice: PracticePrefs { extra_latency_ms: 15.0, tolerance_ms: 80.0, strictness: 0.7 },
            tab_library_dir: Some(PathBuf::from("/tabs")),
            spotify_tab_links: BTreeMap::from([("4uLU6hMCjMI75M1A2tKUQC".to_string(), PathBuf::from("/tabs/acdc.gp5"))]),
            tab_repository_dir: Some(PathBuf::from("/repo")),
            transcription_guitar_model: GuitarModel::Electric,
            transcription_fingering: Fingering::Viterbi,
            transcription_tone: true,
        };
        let json = serde_json::to_string(&settings).expect("serialize");
        let restored: AppSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(settings, restored);
    }

    #[test]
    fn settings_from_older_versions_still_load() {
        // Written before the tab-player fields existed.
        let json = r#"{"input_device":"Mic","output_device":null,"last_preset":null,"buffer_frames":128}"#;
        let restored: AppSettings = serde_json::from_str(json).expect("deserialize");
        assert_eq!(restored.input_device.as_deref(), Some("Mic"));
        assert_eq!(restored.buffer_frames, Some(128));
        assert!(restored.tab_sync.is_empty());
        assert_eq!(restored.practice, PracticePrefs::default());
        assert_eq!(restored.transcription_guitar_model, GuitarModel::Gaps);
        assert_eq!(restored.transcription_fingering, Fingering::Learned);
        assert!(!restored.transcription_tone);
    }

    #[test]
    fn missing_file_yields_default() {
        // load_settings must never panic even if no settings file exists yet;
        // exercised indirectly since we can't isolate `dirs::config_dir()`
        // per-test, so just confirm the fallback path itself doesn't panic.
        let _ = load_settings();
    }
}
