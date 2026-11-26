//! The local tab repository: a folder of Guitar Pro 5 files, each with a
//! `.gvtab.json` sidecar describing where it came from. Filled offline (the
//! "Transcrever áudio" button or the `transcribe` batch tool), and laid out
//! so it can later be synced with a shared repository:
//!
//! ```text
//! <repo>/<Artist> - <Title>.gp5          the tab (opens in any GP-compatible editor)
//! <repo>/<Artist> - <Title>.gvtab.json   TabMetadata: source audio hash, engines, bar times
//! ```
//!
//! Audio is never copied into the repository; the sidecar only remembers
//! the local path (for playback sync) and a SHA-256 of the file (to skip
//! songs already transcribed and, later, to match recordings across users).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::export::score_to_gp5;
use super::model::Score;
use super::timeline::{SyncMap, Timeline};
use crate::shared::paths::app_data_dir;
use crate::shared::preset::sanitize_file_stem;
use crate::trf;

pub const SIDECAR_EXTENSION: &str = "gvtab.json";
pub const METADATA_VERSION: u32 = 1;

pub fn default_repository_dir() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("tab_repository"))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceAudio {
    pub file_name: String,
    /// Where the audio was when transcribed; used to reload it for playback.
    pub path: Option<PathBuf>,
    pub sha256: String,
    pub duration_secs: f64,
    /// YouTube video the audio was downloaded from, if any.
    pub youtube_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackInfo {
    pub name: String,
    /// Which separated stem the track was transcribed from ("mix" if none).
    pub stem: String,
    pub tuning_name: String,
    pub notes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabMetadata {
    pub version: u32,
    pub title: String,
    pub artist: String,
    /// "transcription" for tabs made by the app, "import" for others.
    pub origin: String,
    pub created_unix: u64,
    pub source: SourceAudio,
    /// e.g. "demucs htdemucs_6s", or empty without separation.
    pub separation: String,
    pub transcription: String,
    pub app_version: String,
    pub tracks: Vec<TrackInfo>,
    pub tempo_bpm: f64,
    /// Audio time (seconds) where each bar starts, plus the end of the last.
    pub bar_audio_secs: Vec<f64>,
    pub spotify_track_id: Option<String>,
}

impl TabMetadata {
    /// Score-to-audio sync from the bar times, for `timeline` of this tab.
    pub fn sync_map(&self, timeline: &Timeline) -> Option<SyncMap> {
        let mut points: Vec<(f64, f64)> = self
            .bar_audio_secs
            .iter()
            .enumerate()
            .filter_map(|(bar, &audio)| {
                let score = timeline.nth_occurrence_start(bar, 0).or_else(|| {
                    // The end of the last bar.
                    (bar == timeline.occurrences.len()).then_some(timeline.duration_secs)
                })?;
                Some((score, audio))
            })
            .collect();
        points.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        (points.len() >= 2).then_some(SyncMap::Points(points))
    }
}

pub fn sidecar_path(tab_path: &Path) -> PathBuf {
    tab_path.with_extension(SIDECAR_EXTENSION)
}

pub fn load_metadata(tab_path: &Path) -> Option<TabMetadata> {
    let text = std::fs::read_to_string(sidecar_path(tab_path)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Every tab in `dir` with a readable sidecar: `(tab path, metadata)`.
fn tabs(dir: &Path) -> impl Iterator<Item = (PathBuf, TabMetadata)> + '_ {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(move |path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let stem = name.strip_suffix(&format!(".{SIDECAR_EXTENSION}"))?;
            let meta: TabMetadata =
                serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some((dir.join(format!("{stem}.gp5")), meta))
        })
}

/// First tab in `dir` whose sidecar satisfies `matches`.
fn find_tab(dir: &Path, matches: impl Fn(&TabMetadata) -> bool) -> Option<PathBuf> {
    tabs(dir).find_map(|(path, meta)| matches(&meta).then_some(path))
}

/// Tabs in `dir` made from YouTube videos, by video ID (one folder scan,
/// for marking search results that already have a tab).
pub fn youtube_tabs(dir: &Path) -> HashMap<String, PathBuf> {
    tabs(dir)
        .filter(|(path, _)| path.is_file())
        .filter_map(|(path, meta)| Some((meta.source.youtube_id?, path)))
        .collect()
}

/// Every tab in `dir` that has a sidecar and still exists, newest first
/// (by `created_unix`, then path). Reads every sidecar: worker threads only.
pub fn list_tabs(dir: &Path) -> Vec<(PathBuf, TabMetadata)> {
    let mut found: Vec<(PathBuf, TabMetadata)> = tabs(dir).filter(|(path, _)| path.is_file()).collect();
    found.sort_by(|a, b| b.1.created_unix.cmp(&a.1.created_unix).then_with(|| a.0.cmp(&b.0)));
    found
}

/// Tab already made from audio with this hash, if any.
pub fn find_by_audio_hash(dir: &Path, sha256: &str) -> Option<PathBuf> {
    find_tab(dir, |m| m.source.sha256 == sha256)
}

/// Tab already made from this YouTube video, if any.
pub fn find_by_youtube_id(dir: &Path, video_id: &str) -> Option<PathBuf> {
    find_tab(dir, |m| m.source.youtube_id.as_deref() == Some(video_id))
}

fn display_stem(meta: &TabMetadata) -> String {
    let title = if meta.title.trim().is_empty() {
        "Sem título"
    } else {
        meta.title.trim()
    };
    let raw = if meta.artist.trim().is_empty() {
        title.to_string()
    } else {
        format!("{} - {title}", meta.artist.trim())
    };
    // Keep spaces and dashes readable; only strip what file systems reject.
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        sanitize_file_stem(&raw)
    } else {
        cleaned
    }
}

/// Writes the tab and its sidecar into `dir`. A tab made from the same
/// audio file is replaced; a different song with the same name gets a
/// numbered file. Returns the `.gp5` path.
pub fn save(dir: &Path, score: &Score, meta: &TabMetadata) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| trf!("couldn't create {}: {e}", "não foi possível criar {}: {e}", dir.display()))?;
    let bytes = score_to_gp5(score)?;
    let path = match find_by_audio_hash(dir, &meta.source.sha256)
        .filter(|_| !meta.source.sha256.is_empty())
    {
        Some(existing) => existing,
        None => {
            let stem = display_stem(meta);
            let mut candidate = dir.join(format!("{stem}.gp5"));
            let mut n = 2;
            while candidate.exists() || sidecar_path(&candidate).exists() {
                candidate = dir.join(format!("{stem} ({n}).gp5"));
                n += 1;
            }
            candidate
        }
    };
    let json = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    let temp = path.with_extension("gp5.part");
    std::fs::write(&temp, bytes).map_err(|e| trf!("error saving the tab: {e}", "erro ao gravar a tab: {e}"))?;
    std::fs::rename(&temp, &path).map_err(|e| trf!("error saving the tab: {e}", "erro ao gravar a tab: {e}"))?;
    std::fs::write(sidecar_path(&path), json)
        .map_err(|e| trf!("error saving the metadata: {e}", "erro ao gravar os metadados: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::import::load_score_file;
    use crate::shared::tab::model::{MasterBar, ScoreTrack, TabBeat, TabNote, TrackBar};

    fn score() -> Score {
        let beat = TabBeat {
            duration: 3840,
            value: 1,
            notes: vec![TabNote {
                string: 6,
                fret: 3,
                velocity: 90,
                ..Default::default()
            }],
            ..Default::default()
        };
        Score {
            title: "Song".into(),
            artist: "AC/DC".into(),
            initial_tempo: 120.0,
            bars: vec![
                MasterBar {
                    numerator: 4,
                    denominator: 4,
                    tempo_changes: vec![(0, 120.0)],
                    ..Default::default()
                };
                2
            ],
            tracks: vec![ScoreTrack {
                name: "Guitarra (IA)".into(),
                tuning: vec![64, 59, 55, 50, 45, 40],
                program: 29,
                volume: 100,
                bars: vec![
                    TrackBar {
                        voices: vec![vec![beat]],
                    },
                    TrackBar::default(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn meta(hash: &str) -> TabMetadata {
        TabMetadata {
            version: METADATA_VERSION,
            title: "Song".into(),
            artist: "AC/DC".into(),
            origin: "transcription".into(),
            source: SourceAudio {
                sha256: hash.into(),
                ..Default::default()
            },
            bar_audio_secs: vec![1.0, 3.1, 5.0],
            ..Default::default()
        }
    }

    #[test]
    fn saves_loadable_tabs_with_sidecars_and_dedups_by_audio() {
        let dir = std::env::temp_dir().join(format!("gva_repo_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = save(&dir, &score(), &meta("aaa")).unwrap();
        assert_eq!(first.file_name().unwrap(), "AC_DC - Song.gp5");
        assert_eq!(
            load_score_file(&first).unwrap().tracks[0].bars[0].voices[0][0].notes[0].fret,
            3
        );
        assert_eq!(load_metadata(&first).unwrap().source.sha256, "aaa");
        // Same audio again: same file. Other audio, same name: numbered.
        assert_eq!(save(&dir, &score(), &meta("aaa")).unwrap(), first);
        let second = save(&dir, &score(), &meta("bbb")).unwrap();
        assert_eq!(second.file_name().unwrap(), "AC_DC - Song (2).gp5");
        assert_eq!(find_by_audio_hash(&dir, "bbb"), Some(second));
        assert_eq!(find_by_audio_hash(&dir, "ccc"), None);
        let mut from_video = meta("ddd");
        from_video.source.youtube_id = Some("M4uG1MmZVl0".into());
        let third = save(&dir, &score(), &from_video).unwrap();
        assert_eq!(find_by_youtube_id(&dir, "M4uG1MmZVl0"), Some(third.clone()));
        assert_eq!(find_by_youtube_id(&dir, "xxxxxxxxxxx"), None);
        let by_video = youtube_tabs(&dir);
        assert_eq!(by_video.len(), 1);
        assert_eq!(by_video.get("M4uG1MmZVl0"), Some(&third));
        assert!(youtube_tabs(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_tabs_newest_first() {
        let dir = std::env::temp_dir().join(format!("gva_repo_list_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut old = meta("old");
        old.title = "Old".into();
        old.created_unix = 100;
        let mut new = meta("new");
        new.title = "New".into();
        new.created_unix = 200;
        let old_path = save(&dir, &score(), &old).unwrap();
        let new_path = save(&dir, &score(), &new).unwrap();
        let listed: Vec<PathBuf> = list_tabs(&dir).into_iter().map(|(p, _)| p).collect();
        assert_eq!(listed, vec![new_path.clone(), old_path]);
        // A sidecar whose tab is gone isn't listed.
        std::fs::remove_file(&new_path).unwrap();
        assert_eq!(list_tabs(&dir).len(), 1);
        assert!(list_tabs(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bar_times_become_a_sync_map() {
        let s = score();
        let timeline = Timeline::build(&s);
        let sync = meta("x").sync_map(&timeline).unwrap();
        // Bar 2 starts at score 2.0 s and audio 3.1 s.
        assert!((sync.score_to_audio(2.0) - 3.1).abs() < 1e-9);
        assert!((sync.score_to_audio(0.0) - 1.0).abs() < 1e-9);
        assert!((sync.audio_to_score(5.0) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn hashes_files() {
        let path = std::env::temp_dir().join(format!("gva_hash_{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }
}
