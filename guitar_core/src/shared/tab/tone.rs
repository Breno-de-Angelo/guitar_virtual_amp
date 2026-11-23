//! Tone suggestions for a tab track.
//!
//! There's no data source for "the guitar settings used on this recording":
//! Spotify gives no audio to analyse (and its policy forbids analysing it),
//! and Guitar Pro files only say which instrument sound a track uses. So the
//! suggestion is a starting point, from two sources:
//! - user presets whose `song`/`artist` metadata names the song;
//! - a factory preset picked from the track's instrument (General MIDI
//!   program), name ("Lead", "Clean", "Distortion"...), tuning and how much
//!   of it is palm-muted.

use super::library::words;
use super::model::ScoreTrack;
use crate::shared::preset::Preset;

#[derive(Clone, Debug, PartialEq)]
pub struct ToneGuess {
    /// Name of a factory preset (see `shared::factory_presets`).
    pub factory_preset: &'static str,
    pub reason: String,
}

/// Share of the track's notes that are palm-muted.
fn palm_mute_ratio(track: &ScoreTrack) -> f64 {
    let (mut muted, mut total) = (0usize, 0usize);
    for bar in &track.bars {
        for beat in bar.voices.iter().flatten() {
            for note in &beat.notes {
                total += 1;
                if note.palm_mute || beat.palm_mute {
                    muted += 1;
                }
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        muted as f64 / total as f64
    }
}

/// Picks a factory preset for a guitar track. `None` for tracks that aren't
/// guitar (drums, bass, keys...).
pub fn guess_tone(track: &ScoreTrack) -> Option<ToneGuess> {
    let guitar_program = (24..=31).contains(&track.program);
    if track.percussion || !guitar_program {
        return None;
    }
    let name = track.name.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| name.contains(k));
    let lowest = track.tuning.iter().copied().min().unwrap_or(40);
    let palm_muted = palm_mute_ratio(track);
    let label = if track.name.is_empty() {
        "sem nome".to_string()
    } else {
        track.name.clone()
    };

    let (preset, why) =
        if track.program <= 25 || has(&["acoustic", "acústic", "nylon", "violão", "violao"]) {
            ("Acoustic Simulation", "violão / guitarra acústica")
        } else if track.program == 26 || has(&["jazz"]) {
            ("Clean Jazz", "guitarra jazz")
        } else if has(&["funk", "wah"]) {
            ("Funk Wah", "guitarra funk / wah")
        } else if track.program == 27 || has(&["clean", "limpa", "limpo"]) {
            ("Pop Clean", "timbre limpo")
        } else if has(&["lead", "solo"]) || track.program == 31 {
            ("Rock Lead", "guitarra solo")
        } else if track.program == 30 || has(&["dist", "metal", "heavy", "high gain"]) {
            if lowest < 40 {
                ("Metal Djent", "distorção com afinação baixa")
            } else {
                ("Metal High Gain", "distorção pesada")
            }
        } else if palm_muted > 0.3 {
            ("Classic Rock Rhythm", "base com muito palm mute")
        } else {
            ("Blues Crunch", "overdrive leve")
        };
    let reason = format!("faixa \"{label}\": {why} (programa GM {})", track.program);
    Some(ToneGuess {
        factory_preset: preset,
        reason,
    })
}

/// `true` when the preset's song/artist metadata names this song. A preset
/// with only an artist matches every song by that artist.
pub fn preset_fits_song(preset: &Preset, title: &str, artist: &str) -> bool {
    let contains_all = |haystack: &str, needle: &str| {
        let hay = words(haystack);
        let need = words(needle);
        !need.is_empty() && need.iter().all(|w| hay.contains(w))
    };
    let song_ok = match preset.song.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(song) => contains_all(title, song) || contains_all(song, title),
        None => true,
    };
    let artist_ok = match preset.artist.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(a) => contains_all(a, artist) || contains_all(artist, a),
        None => true,
    };
    let has_metadata = preset.song.as_deref().is_some_and(|s| !s.trim().is_empty())
        || preset
            .artist
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
    has_metadata && song_ok && artist_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::factory_presets::factory_presets;
    use crate::shared::tab::model::{TabBeat, TabNote, TrackBar};

    fn track(name: &str, program: u8, tuning: Vec<u8>) -> ScoreTrack {
        ScoreTrack {
            name: name.into(),
            program,
            tuning,
            ..Default::default()
        }
    }

    const E_STANDARD: [u8; 6] = [64, 59, 55, 50, 45, 40];

    #[test]
    fn guesses_follow_instrument_name_and_tuning() {
        let guess = |t: ScoreTrack| guess_tone(&t).map(|g| g.factory_preset);
        assert_eq!(
            guess(track("Distortion Guitar", 30, E_STANDARD.to_vec())),
            Some("Metal High Gain")
        );
        assert_eq!(
            guess(track("Rhythm", 30, vec![62, 57, 53, 48, 43, 36])),
            Some("Metal Djent")
        );
        assert_eq!(
            guess(track("Lead Guitar", 29, E_STANDARD.to_vec())),
            Some("Rock Lead")
        );
        assert_eq!(
            guess(track("Guitar", 27, E_STANDARD.to_vec())),
            Some("Pop Clean")
        );
        assert_eq!(
            guess(track("Violão", 25, E_STANDARD.to_vec())),
            Some("Acoustic Simulation")
        );
        assert_eq!(guess(track("Bass", 33, vec![43, 38, 33, 28])), None);
        let drums = ScoreTrack {
            percussion: true,
            ..track("Drums", 0, vec![])
        };
        assert_eq!(guess(drums), None);
    }

    #[test]
    fn palm_muted_overdrive_is_a_rock_rhythm() {
        let muted = TabNote {
            string: 6,
            palm_mute: true,
            ..Default::default()
        };
        let beat = TabBeat {
            notes: vec![muted.clone(), muted],
            ..Default::default()
        };
        let mut t = track("Guitar 1", 29, E_STANDARD.to_vec());
        t.bars = vec![TrackBar {
            voices: vec![vec![beat]],
        }];
        assert_eq!(
            guess_tone(&t).unwrap().factory_preset,
            "Classic Rock Rhythm"
        );
        t.bars.clear();
        assert_eq!(guess_tone(&t).unwrap().factory_preset, "Blues Crunch");
    }

    #[test]
    fn every_guess_names_an_existing_factory_preset() {
        let names: Vec<String> = factory_presets().into_iter().map(|p| p.name).collect();
        let tracks = [
            track("a", 24, vec![]),
            track("a", 26, vec![]),
            track("funk", 27, vec![]),
            track("a", 27, vec![]),
            track("a", 31, vec![]),
            track("a", 30, vec![40]),
            track("a", 30, vec![36]),
            track("a", 29, vec![]),
        ];
        for t in tracks {
            let guess = guess_tone(&t).unwrap();
            assert!(
                names.iter().any(|n| n == guess.factory_preset),
                "{} missing",
                guess.factory_preset
            );
        }
    }

    fn preset(song: Option<&str>, artist: Option<&str>) -> Preset {
        Preset {
            name: "p".into(),
            author: None,
            pedals: vec![],
            tags: vec![],
            song: song.map(str::to_string),
            artist: artist.map(str::to_string),
        }
    }

    #[test]
    fn presets_match_by_song_and_artist_metadata() {
        assert!(preset_fits_song(
            &preset(Some("Sweet Child O' Mine"), None),
            "Sweet Child O Mine",
            "Guns N' Roses"
        ));
        assert!(preset_fits_song(
            &preset(None, Some("Guns N Roses")),
            "Paradise City",
            "Guns N' Roses"
        ));
        assert!(!preset_fits_song(
            &preset(Some("November Rain"), None),
            "Paradise City",
            "Guns N' Roses"
        ));
        assert!(!preset_fits_song(
            &preset(None, None),
            "Paradise City",
            "Guns N' Roses"
        ));
        assert!(!preset_fits_song(
            &preset(Some("One"), Some("U2")),
            "One",
            "Metallica"
        ));
    }
}
