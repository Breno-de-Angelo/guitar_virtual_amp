//! Loads Guitar Pro files (GP3, GP4, GP5, GPX, GP7/8) via the `guitarpro`
//! crate and flattens them into [`Score`].

use std::path::Path;

use guitarpro::model::legacy::effects::BendEffect;
use guitarpro::model::legacy::key_signature::Duration;
use guitarpro::{BeatStatus, BendType, NoteType, Song};

use crate::shared::i18n::tr;
use crate::trf;

use super::model::{
    FileSyncPoint, MasterBar, Score, ScoreTrack, TabBeat, TabNote, TrackBar, TICKS_PER_QUARTER,
};

/// Sample rate Guitar Pro 8 uses for `SyncPoint` frame offsets.
const GP_SYNC_FRAME_RATE: f64 = 44_100.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabFormat {
    Gp3,
    Gp4,
    Gp5,
    Gpx,
    Gp,
}

/// File extensions accepted by [`load_score_file`], for file-picker filters.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["gp3", "gp4", "gp5", "gpx", "gp"];

/// Picks the format from the file contents, falling back to the extension.
/// Content sniffing matters because tabs found online are often renamed.
pub fn detect_format(data: &[u8], extension: Option<&str>) -> Option<TabFormat> {
    if data.starts_with(b"PK") {
        return Some(TabFormat::Gp);
    }
    if data.starts_with(b"BCFZ") || data.starts_with(b"BCFS") {
        return Some(TabFormat::Gpx);
    }
    // GP3-5 start with a length-prefixed version string such as
    // "FICHIER GUITAR PRO v5.10".
    if data.len() > 31 {
        let header = String::from_utf8_lossy(&data[1..31]);
        if header.contains("v5.") {
            return Some(TabFormat::Gp5);
        }
        if header.contains("v4.") {
            return Some(TabFormat::Gp4);
        }
        if header.contains("v3.") {
            return Some(TabFormat::Gp3);
        }
    }
    match extension?.to_ascii_lowercase().as_str() {
        "gp3" => Some(TabFormat::Gp3),
        "gp4" => Some(TabFormat::Gp4),
        "gp5" => Some(TabFormat::Gp5),
        "gpx" => Some(TabFormat::Gpx),
        "gp" => Some(TabFormat::Gp),
        _ => None,
    }
}

pub fn load_score_file(path: &Path) -> Result<Score, String> {
    let data = std::fs::read(path).map_err(|e| trf!("Couldn't read the file: {e}", "Não foi possível ler o arquivo: {e}"))?;
    let extension = path.extension().and_then(|e| e.to_str());
    load_score_bytes(&data, extension)
}

pub fn load_score_bytes(data: &[u8], extension: Option<&str>) -> Result<Score, String> {
    let format = detect_format(data, extension)
        .ok_or_else(|| tr("Unknown format (expected GP3/GP4/GP5/GPX/GP)", "Formato não reconhecido (esperado GP3/GP4/GP5/GPX/GP)").to_string())?;
    let mut song = Song::default();
    let result = match format {
        TabFormat::Gp3 => song.read_gp3(data),
        TabFormat::Gp4 => song.read_gp4(data),
        TabFormat::Gp5 => song.read_gp5(data),
        TabFormat::Gpx => song.read_gpx(data),
        TabFormat::Gp => song.read_gp(data),
    };
    result.map_err(|e| trf!("Error reading the Guitar Pro file: {e}", "Erro ao ler o arquivo Guitar Pro: {e}"))?;
    let gpif_tunings = match format {
        TabFormat::Gp => guitarpro::io::gpx::read_gp(data).ok().map(|g| gpif_track_tunings(&g)),
        TabFormat::Gpx => guitarpro::io::gpx::read_gpx(data).ok().map(|g| gpif_track_tunings(&g)),
        _ => None,
    };
    Ok(score_from_song(song, gpif_tunings))
}

/// Per-track tunings (lowest string first) straight from the GPIF XML.
/// guitarpro 0.5.0 parses these but drops GP7 staff tunings when converting
/// to `Song`, falling back to standard guitar tuning for every track.
fn gpif_track_tunings(gpif: &guitarpro::io::gpif::Gpif) -> Vec<Option<Vec<u8>>> {
    let parse = |props: &[guitarpro::io::gpif::Property]| {
        props
            .iter()
            .find(|p| p.name == "Tuning")
            .and_then(|p| p.pitches.as_ref())
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|v| v.parse::<i32>().ok())
                    .map(|v| v.clamp(0, 127) as u8)
                    .collect::<Vec<u8>>()
            })
            .filter(|t| !t.is_empty())
    };
    gpif.tracks
        .tracks
        .iter()
        .map(|t| {
            t.properties
                .as_ref()
                .and_then(|p| parse(&p.properties))
                .or_else(|| {
                    t.staves.as_ref()?.staves.iter().find_map(|s| {
                        s.properties.as_ref().and_then(|p| parse(&p.properties))
                    })
                })
        })
        .collect()
}

fn looks_like_drums(name: &str) -> bool {
    let name = name.to_lowercase();
    ["drum", "percussion", "bateria", "kit"].iter().any(|w| name.contains(w))
}

/// Best-effort General MIDI program for a track whose file doesn't tell us
/// (see the GP3-5 note in `score_from_song`), from its name and tuning.
fn guess_program(name: &str, tuning: &[u8]) -> u8 {
    let name = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    // Bass tunings: at most 6 strings and the highest string at or below G2.
    let bass_tuning = tuning.len() <= 6 && tuning.first().is_some_and(|p| *p <= 43);
    if has(&["bass"]) || bass_tuning {
        33 // Electric Bass (finger)
    } else if has(&["nylon", "classical"]) {
        24
    } else if has(&["acoustic", "violão"]) {
        25
    } else if has(&["piano", "keys", "keyboard"]) {
        0
    } else if has(&["organ"]) {
        16
    } else if has(&["string", "violin", "cello", "orchestr"]) {
        48
    } else if has(&["vocal", "voice", "vox", "melody", "sing", "choir"]) {
        53 // Voice Oohs
    } else if has(&["flute"]) {
        73
    } else if has(&["sax"]) {
        65
    } else if has(&["trumpet", "brass", "horn"]) {
        61
    } else if has(&["synth", "pad"]) {
        89
    } else if has(&["clean"]) {
        27
    } else if has(&["dist", "lead", "solo", "rhythm", "rythm", "metal"]) {
        30 // Distortion Guitar
    } else {
        29 // Overdriven Guitar
    }
}

/// Ticks for a notated duration, applying dots and tuplets.
/// Highest point of a bend, in quarter tones.
fn bend_height(bend: Option<&BendEffect>) -> u8 {
    bend.and_then(|b| b.points.iter().map(|p| p.value.max(0) as u8).max())
        .unwrap_or(0)
}

/// The bend ends lower than its peak (a release).
fn bend_releases(bend: Option<&BendEffect>) -> bool {
    let Some(b) = bend else {
        return false;
    };
    matches!(b.kind, BendType::BendRelease | BendType::PrebendRelease)
        || b.points
            .last()
            .is_some_and(|p| (p.value.max(0) as u8) < bend_height(bend))
}

fn duration_ticks(d: &Duration) -> i64 {
    let value = d.value.max(1) as i64;
    let mut ticks = TICKS_PER_QUARTER * 4 / value;
    if d.double_dotted {
        ticks = ticks * 7 / 4;
    } else if d.dotted {
        ticks = ticks * 3 / 2;
    }
    if d.tuplet_enters > 1 && d.tuplet_times > 0 {
        ticks = ticks * d.tuplet_times as i64 / d.tuplet_enters as i64;
    }
    ticks.max(1)
}

/// `gpif_tunings` is `Some` for GPIF-based files (GPX/GP), holding each
/// track's tuning as read from the XML (see [`gpif_track_tunings`]).
fn score_from_song(mut song: Song, gpif_tunings: Option<Vec<Option<Vec<u8>>>>) -> Score {
    let gpif_based = gpif_tunings.is_some();
    let mut bars: Vec<MasterBar> = song
        .measure_headers
        .iter()
        .map(|h| {
            let mut tempo_changes = Vec::new();
            if h.tempo > 0 {
                tempo_changes.push((0, h.tempo as f64));
            }
            // GP3-5 store "number of jump-backs" here; the GPIF importer
            // stores the total play count instead (GPIF's `count` attribute).
            let repeat_jumps = if h.repeat_close > 0 {
                if gpif_based {
                    (h.repeat_close as u32).saturating_sub(1).max(1)
                } else {
                    h.repeat_close as u32
                }
            } else {
                0
            };
            MasterBar {
                numerator: h.time_signature.numerator.max(1) as u32,
                denominator: h.time_signature.denominator.value.max(1) as u32,
                tempo_changes,
                repeat_open: h.repeat_open,
                repeat_jumps,
                alternatives: h.repeat_alternative,
                marker: h.marker.as_ref().map(|m| m.title.clone()),
            }
        })
        .collect();

    let channels = std::mem::take(&mut song.channels);
    let mut tracks = Vec::with_capacity(song.tracks.len());
    for (track_index, track) in song.tracks.iter().enumerate() {
        let mut strings = track.strings.clone();
        strings.sort_by_key(|(number, _)| *number);
        let mut tuning: Vec<u8> = strings.iter().map(|(_, pitch)| (*pitch).max(0) as u8).collect();
        if let Some(Some(xml_tuning)) = gpif_tunings.as_ref().and_then(|t| t.get(track_index)) {
            tuning = xml_tuning.clone();
        }
        // GPIF numbers strings from the lowest one; GP3-5 (and this model)
        // from the highest. Normalize to "string 1 = highest".
        if gpif_based {
            tuning.reverse();
        }
        let string_count = tuning.len().max(1) as i8;
        let normalize_string = |s: i8| -> u8 {
            if gpif_based { (string_count + 1 - s).max(1) as u8 } else { s.max(1) as u8 }
        };

        // guitarpro 0.5.0 doesn't record which MIDI channel a GP3-5 track
        // uses (`channel_index` stays 0, and GP5 flags percussion by track
        // number) and doesn't read GP7's <Sounds>, so the instrument is
        // usually inferred from the track name/tuning, and drums from their
        // all-zero tuning.
        let zero_tuning = !tuning.is_empty() && tuning.iter().all(|p| *p == 0);
        let percussion = (track.percussion_track && gpif_based)
            || zero_tuning
            || (gpif_based && looks_like_drums(&track.name));
        let program = match track.midi_program_gpif {
            Some(p) if gpif_based => p.clamp(0, 127) as u8,
            _ => guess_program(&track.name, &tuning),
        };
        let volume = if gpif_based {
            channels
                .get(track.channel_index)
                .map(|c| c.volume.clamp(0, 127) as u8)
                .filter(|v| *v > 0)
                .unwrap_or(100)
        } else {
            100
        };

        let mut track_bars = Vec::with_capacity(bars.len());
        for (bar_index, measure) in track.measures.iter().enumerate() {
            let mut voices = Vec::with_capacity(measure.voices.len());
            for voice in &measure.voices {
                let mut cursor: i64 = 0;
                let mut beats = Vec::with_capacity(voice.beats.len());
                for beat in &voice.beats {
                    let duration = duration_ticks(&beat.duration);
                    let start = cursor;
                    cursor += duration;

                    if let Some(mix) = &beat.effect.mix_table_change
                        && let Some(tempo) = &mix.tempo
                        && tempo.value > 0
                        && let Some(bar) = bars.get_mut(bar_index)
                    {
                        bar.tempo_changes.push((start, tempo.value as f64));
                    }

                    if beat.status == BeatStatus::Empty {
                        continue;
                    }

                    let notes: Vec<TabNote> = if beat.status == BeatStatus::Rest {
                        Vec::new()
                    } else {
                        beat.notes
                            .iter()
                            .filter(|n| n.kind != NoteType::Rest && n.string >= 1)
                            .map(|n| TabNote {
                                string: normalize_string(n.string),
                                fret: n.value,
                                velocity: n.velocity.clamp(1, 127) as u8,
                                tie: n.kind == NoteType::Tie,
                                dead: n.kind == NoteType::Dead,
                                ghost: n.effect.ghost_note,
                                hammer: n.effect.hammer,
                                slide: !n.effect.slides.is_empty(),
                                bend: n.effect.bend.is_some(),
                                bend_value: bend_height(n.effect.bend.as_ref()),
                                bend_release: bend_releases(n.effect.bend.as_ref()),
                                vibrato: n.effect.vibrato || beat.effect.vibrato,
                                harmonic: n.effect.harmonic.is_some(),
                                palm_mute: n.effect.palm_mute,
                                let_ring: n.effect.let_ring,
                                staccato: n.effect.staccato,
                            })
                            .collect()
                    };

                    beats.push(TabBeat {
                        start,
                        duration,
                        value: beat.duration.value.max(1),
                        dotted: beat.duration.dotted || beat.duration.double_dotted,
                        tuplet: (beat.duration.tuplet_enters > 1)
                            .then_some(beat.duration.tuplet_enters),
                        palm_mute: notes.iter().any(|n| n.palm_mute),
                        let_ring: notes.iter().any(|n| n.let_ring),
                        text: beat.text.clone(),
                        notes,
                    });
                }
                voices.push(beats);
            }
            track_bars.push(TrackBar { voices });
        }
        // Some files have fewer measures in a track than headers; pad so every
        // track has exactly one `TrackBar` per `MasterBar`.
        track_bars.resize_with(bars.len(), TrackBar::default);

        tracks.push(ScoreTrack {
            name: track.name.trim().to_string(),
            tuning,
            percussion,
            program,
            capo: track.offset.max(0),
            volume,
            bars: track_bars,
        });
    }

    for bar in &mut bars {
        bar.tempo_changes.sort_by_key(|(tick, _)| *tick);
        // Several tracks can carry the same tempo change; keep one per tick.
        bar.tempo_changes.dedup_by_key(|(tick, _)| *tick);
    }

    let sync_points = song
        .sync_points
        .iter()
        .filter_map(|p| {
            let frames = p.frame_offset?;
            Some(FileSyncPoint {
                bar: p.bar_index.max(0) as usize,
                occurrence: p.bar_occurrence.max(0) as usize,
                audio_secs: frames as f64 / GP_SYNC_FRAME_RATE,
            })
        })
        .collect();

    Score {
        title: song.name.trim().to_string(),
        artist: song.artist.trim().to_string(),
        initial_tempo: if song.tempo > 0 { song.tempo as f64 } else { 120.0 },
        bars,
        tracks,
        sync_points,
        backing_audio: song.backing_track_audio.take(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_format_from_magic_bytes_before_extension() {
        assert_eq!(detect_format(b"PK\x03\x04rest", Some("gp5")), Some(TabFormat::Gp));
        assert_eq!(detect_format(b"BCFZ....", None), Some(TabFormat::Gpx));
        let mut gp5 = vec![24u8];
        gp5.extend_from_slice(b"FICHIER GUITAR PRO v5.10      ");
        gp5.extend_from_slice(&[0; 8]);
        assert_eq!(detect_format(&gp5, Some("gp3")), Some(TabFormat::Gp5));
        assert_eq!(detect_format(b"???", Some("GP4")), Some(TabFormat::Gp4));
        assert_eq!(detect_format(b"???", Some("txt")), None);
    }

    #[test]
    fn duration_ticks_applies_dots_and_tuplets() {
        let quarter = Duration { value: 4, ..Default::default() };
        assert_eq!(duration_ticks(&quarter), 960);
        let dotted_eighth = Duration { value: 8, dotted: true, ..Default::default() };
        assert_eq!(duration_ticks(&dotted_eighth), 720);
        let triplet_eighth =
            Duration { value: 8, tuplet_enters: 3, tuplet_times: 2, ..Default::default() };
        assert_eq!(duration_ticks(&triplet_eighth), 320);
    }

    /// Optional smoke test against real files: set `GVA_TAB_SAMPLES` to a
    /// directory of Guitar Pro files to check that every one of them loads.
    /// Skipped otherwise, since no third-party tabs are checked into the repo.
    #[test]
    fn loads_sample_directory_when_provided() {
        let Ok(dir) = std::env::var("GVA_TAB_SAMPLES") else {
            return;
        };
        for entry in std::fs::read_dir(dir).expect("read sample dir") {
            let path = entry.expect("dir entry").path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
                continue;
            }
            let score = load_score_file(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!score.bars.is_empty(), "{}: no bars", path.display());
            assert!(!score.tracks.is_empty(), "{}: no tracks", path.display());
            for track in &score.tracks {
                assert_eq!(track.bars.len(), score.bars.len());
            }
        }
    }
}
