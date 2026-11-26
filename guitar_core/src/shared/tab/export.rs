//! Writes a [`Score`] as a Guitar Pro 5 file, the format of the local tab
//! repository: it opens in Guitar Pro, TuxGuitar and MuseScore, and reads
//! back through `import` unchanged.
//!
//! GP5 stores whole-number tempos (one per bar, as a mix-table change on the
//! bar's first beat), so fractional tempos are rounded; the repository's
//! sidecar keeps exact bar times for audio sync (see `shared::tab::repository`).

use guitarpro::audio::midi::MidiChannel;
use guitarpro::model::legacy::effects::{BendEffect, BendPoint};
use guitarpro::model::legacy::key_signature::{Duration, TimeSignature};
use guitarpro::model::legacy::mix_table::{MixTableChange, MixTableItem};
use guitarpro::{
    Beat, BeatStatus, BendType, Measure, MeasureHeader, Note, NoteType, SlideType, Song, Track,
    Voice,
};

use crate::shared::i18n::tr;
use crate::trf;

use super::model::{Score, TICKS_PER_QUARTER, TabBeat, TabNote};

/// GP bend of a note: up to `bend_value` quarter tones (a whole step when
/// unknown), held or released halfway through the note.
fn bend_of(n: &TabNote) -> Option<BendEffect> {
    if !n.bend {
        return None;
    }
    let value = if n.bend_value > 0 {
        n.bend_value as i8
    } else {
        4
    };
    let point = |position: u8, value: i8| BendPoint {
        position,
        value,
        vibrato: false,
    };
    let (kind, points) = if n.bend_release {
        (
            BendType::BendRelease,
            vec![
                point(0, 0),
                point(3, value),
                point(6, value),
                point(9, 0),
                point(12, 0),
            ],
        )
    } else {
        (
            BendType::Bend,
            vec![point(0, 0), point(6, value), point(12, value)],
        )
    };
    Some(BendEffect {
        kind,
        // GP's units: 100 per whole step, 25 per quarter tone.
        value: value as i16 * 25,
        points,
        ..BendEffect::default()
    })
}

fn duration_of(beat: &TabBeat) -> Duration {
    let mut duration = Duration {
        value: beat.value.max(1),
        dotted: beat.dotted,
        ..Duration::default()
    };
    if let Some(enters) = beat.tuplet.filter(|n| *n > 1) {
        duration.tuplet_enters = enters;
        // 3:2, 5:4, 6:4, 7:4... (the usual GP tuplets).
        duration.tuplet_times = match enters {
            3 => 2,
            5..=7 => 4,
            9..=13 => 8,
            n => n - 1,
        };
    }
    duration
}

fn rest(value: u16) -> Beat {
    Beat {
        status: BeatStatus::Rest,
        duration: Duration {
            value,
            ..Duration::default()
        },
        ..Beat::default()
    }
}

/// Rounded tempo the bar starts with, if it changes there.
fn bar_tempo(score: &Score, bar: usize) -> Option<i32> {
    score.bars[bar]
        .tempo_changes
        .first()
        .filter(|(tick, _)| *tick == 0)
        .map(|(_, bpm)| bpm.round() as i32)
}

pub fn score_to_gp5(score: &Score) -> Result<Vec<u8>, String> {
    if score.tracks.is_empty() || score.bars.is_empty() {
        return Err(tr("empty score", "partitura vazia").into());
    }
    let mut song = Song {
        name: score.title.clone(),
        artist: score.artist.clone(),
        tempo: score.initial_tempo.round().clamp(20.0, 400.0) as i16,
        ..Song::default()
    };

    let mut start = TICKS_PER_QUARTER;
    for (i, bar) in score.bars.iter().enumerate() {
        song.measure_headers.push(MeasureHeader {
            number: (i + 1) as u16,
            start,
            tempo: bar_tempo(score, i).unwrap_or(0),
            repeat_open: bar.repeat_open,
            repeat_close: if bar.repeat_jumps > 0 {
                bar.repeat_jumps as i8
            } else {
                -1
            },
            repeat_alternative: bar.alternatives,
            time_signature: TimeSignature {
                numerator: bar.numerator.clamp(1, 32) as i8,
                denominator: Duration {
                    value: bar.denominator.max(1) as u16,
                    ..Duration::default()
                },
                beams: vec![2, 2, 2, 2],
            },
            ..MeasureHeader::default()
        });
        start += bar.length_ticks();
    }

    // Channels 0-8 and 10-15 for melodic tracks, 9 for drums.
    let mut next_channel = 0u8;
    for (track_index, track) in score.tracks.iter().enumerate() {
        let channel = if track.percussion {
            9
        } else {
            let c = next_channel;
            next_channel = if next_channel == 8 {
                10
            } else {
                (next_channel + 1).min(15)
            };
            c
        };
        song.channels.push(MidiChannel {
            channel,
            effect_channel: channel,
            instrument: track.program as i32,
            volume: (track.volume.min(127)) as i8,
            ..MidiChannel::default()
        });

        let strings: Vec<(i8, i8)> = if track.percussion {
            (1..=6).map(|s| (s, 0)).collect()
        } else {
            track
                .tuning
                .iter()
                .enumerate()
                .map(|(i, p)| ((i + 1) as i8, *p as i8))
                .collect()
        };
        let mut measures = Vec::with_capacity(score.bars.len());
        let mut header_start = TICKS_PER_QUARTER;
        for (bar_index, bar) in score.bars.iter().enumerate() {
            let source = track.bars.get(bar_index);
            let main: &[TabBeat] = source
                .and_then(|b| b.voices.first())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut beats: Vec<Beat> = main
                .iter()
                .map(|beat| {
                    let notes: Vec<Note> = beat
                        .notes
                        .iter()
                        .map(|n| {
                            let mut note = Note {
                                value: n.fret,
                                velocity: n.velocity.clamp(15, 127) as i16,
                                string: n.string as i8,
                                kind: if n.tie {
                                    NoteType::Tie
                                } else if n.dead {
                                    NoteType::Dead
                                } else {
                                    NoteType::Normal
                                },
                                ..Note::default()
                            };
                            note.effect.palm_mute = n.palm_mute;
                            note.effect.let_ring = n.let_ring;
                            note.effect.hammer = n.hammer;
                            note.effect.ghost_note = n.ghost;
                            note.effect.staccato = n.staccato;
                            note.effect.vibrato = n.vibrato;
                            note.effect.bend = bend_of(n);
                            if n.slide {
                                note.effect.slides = vec![SlideType::ShiftSlideTo];
                            }
                            note
                        })
                        .collect();
                    Beat {
                        status: if notes.is_empty() {
                            BeatStatus::Rest
                        } else {
                            BeatStatus::Normal
                        },
                        duration: duration_of(beat),
                        text: beat.text.clone(),
                        notes,
                        ..Beat::default()
                    }
                })
                .collect();
            if beats.is_empty() {
                // A whole-bar rest (a whole note fits any bar GP accepts).
                beats.push(rest(1));
            }
            // Tempo goes on the first track's first beat of each bar.
            if track_index == 0
                && let Some(bpm) = bar_tempo(score, bar_index)
            {
                beats[0].effect.mix_table_change = Some(MixTableChange {
                    tempo: Some(MixTableItem {
                        value: bpm.clamp(1, 255) as u8,
                        duration: 0,
                        all_tracks: false,
                    }),
                    ..MixTableChange::default()
                });
            }
            let time_signature = song.measure_headers[bar_index].time_signature.clone();
            measures.push(Measure {
                number: bar_index + 1,
                start: header_start,
                track_index,
                header_index: bar_index,
                time_signature,
                voices: vec![
                    Voice {
                        beats,
                        ..Voice::default()
                    },
                    Voice::default(),
                ],
                ..Measure::default()
            });
            header_start += bar.length_ticks();
        }

        song.tracks.push(Track {
            number: (track_index + 1) as i32,
            channel_index: track_index,
            name: track.name.clone(),
            strings,
            percussion_track: track.percussion,
            offset: track.capo,
            measures,
            ..Track::default()
        });
    }

    song.write((5, 1, 0), None)
        .map_err(|e| trf!("error writing the GP5 file: {e}", "erro ao gerar o arquivo GP5: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::import::load_score_bytes;
    use crate::shared::tab::model::{MasterBar, ScoreTrack, TabNote, TrackBar};

    fn note(string: u8, fret: i16) -> TabNote {
        TabNote {
            string,
            fret,
            velocity: 95,
            ..Default::default()
        }
    }

    fn beat(start: i64, value: u16, notes: Vec<TabNote>) -> TabBeat {
        TabBeat {
            start,
            duration: TICKS_PER_QUARTER * 4 / value as i64,
            value,
            notes,
            ..Default::default()
        }
    }

    fn sample() -> Score {
        let guitar = ScoreTrack {
            name: "Guitar".into(),
            tuning: vec![64, 59, 55, 50, 45, 40],
            program: 29,
            volume: 100,
            bars: vec![
                TrackBar {
                    voices: vec![vec![
                        beat(0, 4, vec![note(6, 0), note(5, 2), note(4, 2)]),
                        beat(960, 8, vec![note(3, 4)]),
                        beat(1440, 8, vec![]),
                        beat(
                            1920,
                            2,
                            vec![TabNote {
                                palm_mute: true,
                                ..note(6, 3)
                            }],
                        ),
                    ]],
                },
                TrackBar {
                    voices: vec![vec![
                        beat(
                            0,
                            4,
                            vec![TabNote {
                                bend: true,
                                bend_value: 4,
                                ..note(2, 8)
                            }],
                        ),
                        beat(
                            960,
                            4,
                            vec![TabNote {
                                bend: true,
                                bend_value: 2,
                                bend_release: true,
                                vibrato: true,
                                ..note(3, 7)
                            }],
                        ),
                        beat(
                            1920,
                            4,
                            vec![TabNote {
                                slide: true,
                                ..note(3, 5)
                            }],
                        ),
                        beat(
                            2880,
                            8,
                            vec![TabNote {
                                hammer: true,
                                ..note(3, 7)
                            }],
                        ),
                        beat(3360, 8, vec![note(3, 9)]),
                    ]],
                },
                TrackBar::default(),
            ],
            ..Default::default()
        };
        let bass = ScoreTrack {
            name: "Bass".into(),
            tuning: vec![43, 38, 33, 28],
            program: 33,
            volume: 100,
            bars: vec![
                TrackBar {
                    voices: vec![vec![beat(0, 1, vec![note(4, 0)])]],
                },
                TrackBar::default(),
                TrackBar::default(),
            ],
            ..Default::default()
        };
        Score {
            title: "Song".into(),
            artist: "Band".into(),
            initial_tempo: 123.4,
            bars: vec![
                MasterBar {
                    numerator: 4,
                    denominator: 4,
                    tempo_changes: vec![(0, 123.4)],
                    ..Default::default()
                },
                MasterBar {
                    numerator: 4,
                    denominator: 4,
                    tempo_changes: vec![(0, 98.0)],
                    ..Default::default()
                },
                MasterBar {
                    numerator: 4,
                    denominator: 4,
                    tempo_changes: vec![(0, 98.0)],
                    ..Default::default()
                },
            ],
            tracks: vec![guitar, bass],
            ..Default::default()
        }
    }

    #[test]
    fn round_trips_through_the_importer() {
        let score = sample();
        let bytes = score_to_gp5(&score).unwrap();
        let back = load_score_bytes(&bytes, Some("gp5")).unwrap();
        assert_eq!(back.title, "Song");
        assert_eq!(back.artist, "Band");
        assert_eq!(back.bars.len(), 3);
        assert_eq!(back.tracks.len(), 2);
        let g = &back.tracks[0];
        assert_eq!(g.tuning, vec![64, 59, 55, 50, 45, 40]);
        let beats = &g.bars[0].voices[0];
        assert_eq!(beats.len(), 4);
        // GP5 stores a beat's notes by string, highest first.
        let frets: Vec<(u8, i16)> = beats[0].notes.iter().map(|n| (n.string, n.fret)).collect();
        assert_eq!(frets, vec![(4, 2), (5, 2), (6, 0)]);
        assert_eq!((beats[1].start, beats[1].value), (960, 8));
        assert!(beats[2].notes.is_empty(), "rest kept");
        assert!(beats[3].notes[0].palm_mute);
        assert_eq!(back.tracks[1].tuning, vec![43, 38, 33, 28]);
        assert_eq!(back.tracks[1].bars[0].voices[0][0].notes[0].fret, 0);
        // Tempos come back rounded, one per bar.
        assert_eq!(back.bars[0].tempo_changes.first().map(|t| t.1), Some(123.0));
        assert_eq!(back.bars[1].tempo_changes.first().map(|t| t.1), Some(98.0));
        // The empty bar of the bass is a full-bar rest.
        assert!(
            back.tracks[1].bars[1].voices[0]
                .iter()
                .all(|b| b.notes.is_empty())
        );
    }

    #[test]
    fn techniques_round_trip() {
        let back = load_score_bytes(&score_to_gp5(&sample()).unwrap(), Some("gp5")).unwrap();
        let beats = &back.tracks[0].bars[1].voices[0];
        let n = |i: usize| &beats[i].notes[0];
        assert!(
            n(0).bend && n(0).bend_value == 4 && !n(0).bend_release,
            "{:?}",
            n(0)
        );
        assert!(n(1).bend && n(1).bend_value == 2 && n(1).bend_release && n(1).vibrato);
        assert!(n(2).slide && !n(2).bend);
        assert!(n(3).hammer && !n(4).hammer);
    }

    #[test]
    fn empty_scores_are_refused() {
        assert!(score_to_gp5(&Score::default()).is_err());
    }
}
