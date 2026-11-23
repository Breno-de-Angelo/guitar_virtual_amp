//! On-disk layout for TONE3000 data, under the app data directory:
//!
//! ```text
//! tone3000/session.json                      OAuth tokens (owner-only permissions)
//! tone3000/models/<tone_id>/<model_id>.nam   downloaded model or .wav IR
//! tone3000/models/<tone_id>/<model_id>.json  creator, license and source URL
//! ```
//!
//! Files land here only when the user asks for a model, as the API terms
//! require. Nothing from the catalog is prefetched or mirrored.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::client::Session;
use super::types::{Model, Tone};
use crate::shared::{paths::app_data_dir, pedals::Tone3000Source};

fn tone3000_dir() -> Option<PathBuf> {
    app_data_dir().map(|dir| dir.join("tone3000"))
}

pub fn session_path() -> Option<PathBuf> {
    tone3000_dir().map(|dir| dir.join("session.json"))
}

pub fn models_dir() -> Option<PathBuf> {
    tone3000_dir().map(|dir| dir.join("models"))
}

/// File extension for a tone format. TONE3000 serves IRs as WAV.
pub fn extension_for(format: &str) -> &'static str {
    match format {
        "ir" => "wav",
        _ => "nam",
    }
}

pub fn model_file_path(tone: &Tone, model: &Model) -> Option<PathBuf> {
    models_dir().map(|dir| {
        dir.join(tone.id.to_string())
            .join(format!("{}.{}", model.id, extension_for(&tone.format)))
    })
}

/// Writes `bytes` to `path` so only the current user can read it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub fn load_session(path: &Path) -> Option<Session> {
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

pub fn save_session(path: &Path, session: &Session) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(session).map_err(std::io::Error::other)?;
    write_private(path, &json)
}

/// Writes the model file atomically (temp file + rename, so a failed
/// download never leaves a truncated model behind) plus its sidecar.
pub fn write_model(path: &Path, bytes: &[u8], source: &Tone3000Source) -> std::io::Result<()> {
    let temp = path.with_extension("part");
    write_private(&temp, bytes)?;
    fs::rename(&temp, path)?;
    let sidecar = serde_json::to_vec_pretty(source).map_err(std::io::Error::other)?;
    write_private(&path.with_extension("json"), &sidecar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_with_private_permissions() {
        let dir = std::env::temp_dir().join("guitar_virtual_amp_t3k_storage");
        let path = dir.join("session.json");
        let session = Session {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 123,
        };
        save_session(&path, &session).unwrap();
        assert_eq!(load_session(&path), Some(session));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "session file must not be group/world readable"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ir_tones_are_saved_as_wav() {
        assert_eq!(extension_for("ir"), "wav");
        assert_eq!(extension_for("nam"), "nam");
    }
}
