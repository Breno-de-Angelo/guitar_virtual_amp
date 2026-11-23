//! Playback timeline: expands repeats into a linear bar order, converts ticks
//! to seconds through the tempo map, and generates synthesizer note events.
//!
//! "Score time" below means seconds since the start of the first bar when the
//! score is played at its notated tempo. It's the shared clock between the
//! synth, the tab cursor and the backing-track sync map.

use super::model::{FileSyncPoint, Score, TICKS_PER_QUARTER};

/// Upper bound on expanded bar occurrences, so a malformed repeat structure
/// can't make the expansion loop forever.
const MAX_OCCURRENCES: usize = 20_000;

/// One played instance of a bar (a bar inside a repeat appears several times).
#[derive(Clone, Debug, PartialEq)]
pub struct BarOccurrence {
    /// Index into `Score::bars`.
    pub bar: usize,
    pub start_secs: f64,
    pub end_secs: f64,
    /// Tempo segments inside this occurrence: `(tick in bar, secs, bpm)`,
    /// first entry always at tick 0.
    segments: Vec<(i64, f64, f64)>,
}

impl BarOccurrence {
    pub fn tick_to_secs(&self, tick: i64) -> f64 {
        let idx = self.segments.partition_point(|s| s.0 <= tick).saturating_sub(1);
        let (seg_tick, seg_secs, bpm) = self.segments[idx];
        seg_secs + ticks_to_secs(tick - seg_tick, bpm)
    }

    pub fn secs_to_tick(&self, secs: f64) -> f64 {
        let idx = self.segments.partition_point(|s| s.1 <= secs).saturating_sub(1);
        let (seg_tick, seg_secs, bpm) = self.segments[idx];
        seg_tick as f64 + (secs - seg_secs) * bpm / 60.0 * TICKS_PER_QUARTER as f64
    }
}

fn ticks_to_secs(ticks: i64, bpm: f64) -> f64 {
    ticks as f64 / TICKS_PER_QUARTER as f64 * 60.0 / bpm.max(1.0)
}

/// Expands repeats and alternate endings into the order bars are played in.
pub fn playback_order(score: &Score) -> Vec<usize> {
    let bars = &score.bars;
    let mut order = Vec::with_capacity(bars.len());
    let mut i = 0;
    let mut repeat_start = 0;
    // 1-based pass number through the current repeat section.
    let mut pass: u32 = 1;
    // Jumps already taken at each repeat-close bar during the current section.
    let mut jumps_taken = vec![0u32; bars.len()];
    // True when the current bar was reached by jumping back from a repeat
    // close, as opposed to arriving at it sequentially.
    let mut jumped_back = false;

    while i < bars.len() && order.len() < MAX_OCCURRENCES {
        let bar = &bars[i];
        if bar.repeat_open && !jumped_back {
            // Entering a repeat section from the front.
            repeat_start = i;
            pass = 1;
            jumps_taken.iter_mut().skip(i).for_each(|j| *j = 0);
        }
        jumped_back = false;

        let skip_for_ending =
            bar.alternatives != 0 && pass <= 8 && bar.alternatives & (1 << (pass - 1)) == 0;
        if skip_for_ending {
            i += 1;
            continue;
        }

        order.push(i);

        if bar.repeat_jumps > 0 && jumps_taken[i] < bar.repeat_jumps {
            jumps_taken[i] += 1;
            pass += 1;
            i = repeat_start;
            jumped_back = true;
            continue;
        }
        if bar.repeat_jumps > 0 {
            // Section done; reset so a following section starts fresh.
            pass = 1;
            repeat_start = i + 1;
        }
        i += 1;
    }
    order
}

/// A note-on (`velocity > 0`) or note-off (`velocity == 0`) for the synth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    pub secs: f64,
    pub track: u16,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    pub occurrences: Vec<BarOccurrence>,
    /// Sorted by time; at equal times note-offs come before note-ons so a
    /// re-struck key isn't cut off by its own previous release.
    pub events: Vec<NoteEvent>,
    /// MIDI channel assigned to each track (index = track index).
    pub track_channels: Vec<u8>,
    pub duration_secs: f64,
}

/// Assigns MIDI channels: percussion on 9, everything else round-robin over
/// the 15 melodic channels.
fn assign_channels(score: &Score) -> Vec<u8> {
    let mut next = 0u8;
    score
        .tracks
        .iter()
        .map(|t| {
            if t.percussion {
                9
            } else {
                let ch = next;
                next = (next + 1) % 16;
                if next == 9 {
                    next = 10;
                }
                ch
            }
        })
        .collect()
}

impl Timeline {
    pub fn build(score: &Score) -> Self {
        let order = playback_order(score);
        let mut occurrences = Vec::with_capacity(order.len());
        let mut bpm = score.initial_tempo.max(1.0);
        let mut secs = 0.0;
        for &bar_index in &order {
            let bar = &score.bars[bar_index];
            let length = bar.length_ticks();
            let mut segments = Vec::with_capacity(1 + bar.tempo_changes.len());
            let mut seg_tick = 0;
            let mut seg_secs = secs;
            for &(tick, new_bpm) in &bar.tempo_changes {
                let tick = tick.clamp(0, length);
                if tick > seg_tick {
                    segments.push((seg_tick, seg_secs, bpm));
                    seg_secs += ticks_to_secs(tick - seg_tick, bpm);
                    seg_tick = tick;
                }
                bpm = new_bpm.max(1.0);
            }
            segments.push((seg_tick, seg_secs, bpm));
            let end = seg_secs + ticks_to_secs(length - seg_tick, bpm);
            occurrences.push(BarOccurrence { bar: bar_index, start_secs: secs, end_secs: end, segments });
            secs = end;
        }

        let track_channels = assign_channels(score);
        let mut timeline = Timeline {
            occurrences,
            events: Vec::new(),
            track_channels,
            duration_secs: secs,
        };
        timeline.events = timeline.generate_events(score);
        timeline
    }

    fn generate_events(&self, score: &Score) -> Vec<NoteEvent> {
        let mut events: Vec<NoteEvent> = Vec::new();
        for (track_index, track) in score.tracks.iter().enumerate() {
            let channel = self.track_channels[track_index];
            // Pending note-off per string: (event index into `events`) so a
            // tied note can extend it and a let-ring note can be cut by the
            // next note on the same string.
            // Stored with the note's start time, because voices are visited
            // one after another and must not cut a note that starts later.
            let mut pending: Vec<Option<(usize, f64)>> = vec![None; track.string_count() + 1];
            for occ in &self.occurrences {
                let Some(bar) = track.bars.get(occ.bar) else { continue };
                for voice in &bar.voices {
                    for beat in voice {
                        let on = occ.tick_to_secs(beat.start);
                        let off = occ.tick_to_secs(beat.start + beat.duration);
                        for note in &beat.notes {
                            let string = (note.string as usize).min(track.string_count());
                            if note.tie {
                                if let Some((idx, _)) = pending[string]
                                    && events[idx].secs < off
                                {
                                    events[idx].secs = off;
                                }
                                continue;
                            }
                            // A new pick on this string ends whatever was ringing.
                            if let Some((idx, started)) = pending[string]
                                && started < on
                                && events[idx].secs > on
                            {
                                events[idx].secs = on;
                            }
                            let key = if track.percussion {
                                note.fret as i32
                            } else {
                                let open = track.tuning.get(string - 1).copied().unwrap_or(40) as i32;
                                open + track.capo + note.fret as i32
                            };
                            if !(0..=127).contains(&key) {
                                continue;
                            }
                            let mut velocity = note.velocity as i32;
                            let mut end = off;
                            if note.dead {
                                velocity = velocity * 2 / 3;
                                end = on + (off - on).min(0.06);
                            } else if note.palm_mute || note.staccato {
                                velocity = velocity * 9 / 10;
                                end = on + (off - on) * 0.5;
                            } else if note.ghost {
                                velocity = velocity * 2 / 3;
                            }
                            if note.let_ring {
                                // Ring until the next note on this string (capped).
                                end = off.max(on + 4.0);
                            }
                            let base = NoteEvent {
                                secs: on,
                                track: track_index as u16,
                                channel,
                                key: key as u8,
                                velocity: velocity.clamp(1, 127) as u8,
                            };
                            events.push(base);
                            events.push(NoteEvent { secs: end, velocity: 0, ..base });
                            if !track.percussion {
                                pending[string] = Some((events.len() - 1, on));
                            }
                        }
                    }
                }
            }
        }
        events.sort_by(|a, b| {
            a.secs
                .total_cmp(&b.secs)
                .then_with(|| (a.velocity > 0).cmp(&(b.velocity > 0)))
        });
        events
    }

    /// `(channel, program, volume)` for each MIDI channel in use. When tracks
    /// share a channel (more than 15 melodic tracks), the first one wins.
    pub fn channel_setup(&self, score: &Score) -> Vec<(u8, u8, u8)> {
        let mut setup: Vec<(u8, u8, u8)> = Vec::new();
        for (track, &channel) in score.tracks.iter().zip(&self.track_channels) {
            if !setup.iter().any(|(ch, _, _)| *ch == channel) {
                setup.push((channel, track.program, track.volume));
            }
        }
        setup
    }

    /// Index of the occurrence playing at `secs` (clamped to the song range).
    pub fn occurrence_at(&self, secs: f64) -> Option<usize> {
        if self.occurrences.is_empty() {
            return None;
        }
        let idx = self.occurrences.partition_point(|o| o.start_secs <= secs);
        Some(idx.saturating_sub(1).min(self.occurrences.len() - 1))
    }

    /// The occurrence of `bar` that starts closest to `secs` (so clicking a
    /// bar inside a repeat jumps to the nearest pass, not always the first).
    pub fn closest_occurrence(&self, bar: usize, secs: f64) -> Option<usize> {
        self.occurrences
            .iter()
            .enumerate()
            .filter(|(_, o)| o.bar == bar)
            .min_by(|(_, a), (_, b)| {
                (a.start_secs - secs).abs().total_cmp(&(b.start_secs - secs).abs())
            })
            .map(|(i, _)| i)
    }

    /// Score time at which occurrence number `n` (0-based) of `bar` starts.
    pub fn nth_occurrence_start(&self, bar: usize, n: usize) -> Option<f64> {
        self.occurrences
            .iter()
            .filter(|o| o.bar == bar)
            .nth(n)
            .map(|o| o.start_secs)
    }
}

/// Maps between score time and backing-track ("audio") time.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncMap {
    /// `audio = offset + score / tempo_ratio`. `tempo_ratio` > 1 means the
    /// recording is faster than the notated tempo.
    Linear { offset_secs: f64, tempo_ratio: f64 },
    /// Piecewise-linear anchors `(score secs, audio secs)`, sorted by score
    /// time (from Guitar Pro 8 sync points).
    Points(Vec<(f64, f64)>),
}

impl Default for SyncMap {
    fn default() -> Self {
        SyncMap::Linear { offset_secs: 0.0, tempo_ratio: 1.0 }
    }
}

fn interpolate(points: &[(f64, f64)], x: f64, forward: bool) -> f64 {
    let key = |p: &(f64, f64)| if forward { p.0 } else { p.1 };
    let val = |p: &(f64, f64)| if forward { p.1 } else { p.0 };
    match points.len() {
        0 => x,
        1 => val(&points[0]) + (x - key(&points[0])),
        _ => {
            let idx = points.partition_point(|p| key(p) <= x).clamp(1, points.len() - 1);
            let (a, b) = (&points[idx - 1], &points[idx]);
            let span = key(b) - key(a);
            if span.abs() < 1e-9 {
                return val(a);
            }
            val(a) + (x - key(a)) * (val(b) - val(a)) / span
        }
    }
}

impl SyncMap {
    pub fn score_to_audio(&self, score_secs: f64) -> f64 {
        match self {
            SyncMap::Linear { offset_secs, tempo_ratio } => {
                offset_secs + score_secs / tempo_ratio.max(0.01)
            }
            SyncMap::Points(points) => interpolate(points, score_secs, true),
        }
    }

    pub fn audio_to_score(&self, audio_secs: f64) -> f64 {
        match self {
            SyncMap::Linear { offset_secs, tempo_ratio } => {
                (audio_secs - offset_secs) * tempo_ratio.max(0.01)
            }
            SyncMap::Points(points) => interpolate(points, audio_secs, false),
        }
    }

    /// Builds a map from Guitar Pro 8 sync points, or `None` if none of them
    /// can be resolved against this timeline.
    pub fn from_file_points(points: &[FileSyncPoint], timeline: &Timeline) -> Option<Self> {
        let mut anchors: Vec<(f64, f64)> = points
            .iter()
            .filter_map(|p| {
                let score = timeline.nth_occurrence_start(p.bar, p.occurrence)?;
                Some((score, p.audio_secs))
            })
            .collect();
        anchors.sort_by(|a, b| a.0.total_cmp(&b.0));
        anchors.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        (!anchors.is_empty()).then_some(SyncMap::Points(anchors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::model::{MasterBar, ScoreTrack, TabBeat, TabNote, TrackBar};

    fn bar() -> MasterBar {
        MasterBar { numerator: 4, denominator: 4, ..Default::default() }
    }

    fn score_with_bars(bars: Vec<MasterBar>) -> Score {
        Score { initial_tempo: 120.0, bars, ..Default::default() }
    }

    #[test]
    fn plain_bars_play_in_order() {
        let score = score_with_bars(vec![bar(), bar(), bar()]);
        assert_eq!(playback_order(&score), vec![0, 1, 2]);
    }

    #[test]
    fn simple_repeat_plays_section_twice() {
        let score = score_with_bars(vec![
            bar(),
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { repeat_jumps: 1, ..bar() },
            bar(),
        ]);
        assert_eq!(playback_order(&score), vec![0, 1, 2, 1, 2, 3]);
    }

    #[test]
    fn repeat_without_open_goes_back_to_start() {
        let score = score_with_bars(vec![bar(), MasterBar { repeat_jumps: 2, ..bar() }, bar()]);
        assert_eq!(playback_order(&score), vec![0, 1, 0, 1, 0, 1, 2]);
    }

    #[test]
    fn alternate_endings_are_chosen_per_pass() {
        let score = score_with_bars(vec![
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { alternatives: 0b01, repeat_jumps: 1, ..bar() },
            MasterBar { alternatives: 0b10, ..bar() },
            bar(),
        ]);
        assert_eq!(playback_order(&score), vec![0, 1, 0, 2, 3]);
    }

    #[test]
    fn two_consecutive_repeat_sections() {
        let score = score_with_bars(vec![
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { repeat_jumps: 1, ..bar() },
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { repeat_jumps: 1, ..bar() },
        ]);
        assert_eq!(playback_order(&score), vec![0, 1, 0, 1, 2, 3, 2, 3]);
    }

    #[test]
    fn timing_follows_tempo_changes() {
        let score = score_with_bars(vec![
            bar(),
            MasterBar { tempo_changes: vec![(0, 60.0)], ..bar() },
            MasterBar { tempo_changes: vec![(1920, 120.0)], ..bar() },
        ]);
        let tl = Timeline::build(&score);
        // 4/4 at 120 bpm = 2s; at 60 bpm = 4s; half at 60 + half at 120 = 3s.
        let starts: Vec<f64> = tl.occurrences.iter().map(|o| o.start_secs).collect();
        assert_eq!(starts, vec![0.0, 2.0, 6.0]);
        assert!((tl.duration_secs - 9.0).abs() < 1e-9);
        let last = &tl.occurrences[2];
        assert!((last.tick_to_secs(1920) - 8.0).abs() < 1e-9);
        assert!((last.secs_to_tick(8.5) - 2880.0).abs() < 1e-6);
    }

    fn guitar_track(bars: Vec<TrackBar>) -> ScoreTrack {
        ScoreTrack {
            name: "Guitar".into(),
            tuning: vec![64, 59, 55, 50, 45, 40],
            program: 25,
            volume: 100,
            bars,
            ..Default::default()
        }
    }

    fn quarter(start: i64, notes: Vec<TabNote>) -> TabBeat {
        TabBeat { start, duration: 960, value: 4, notes, ..Default::default() }
    }

    fn note(string: u8, fret: i16) -> TabNote {
        TabNote { string, fret, velocity: 95, ..Default::default() }
    }

    #[test]
    fn events_use_tuning_capo_and_ties() {
        let beats = vec![
            quarter(0, vec![note(6, 3)]),
            quarter(960, vec![TabNote { tie: true, ..note(6, 3) }]),
            quarter(1920, vec![note(1, 0)]),
        ];
        let mut track = guitar_track(vec![TrackBar { voices: vec![beats] }]);
        track.capo = 2;
        let score = Score { tracks: vec![track], ..score_with_bars(vec![bar()]) };
        let tl = Timeline::build(&score);
        let ons: Vec<(f64, u8)> =
            tl.events.iter().filter(|e| e.velocity > 0).map(|e| (e.secs, e.key)).collect();
        // Low E (40) + capo 2 + fret 3 = 45; high E (64) + capo 2 = 66.
        assert_eq!(ons, vec![(0.0, 45), (1.0, 66)]);
        let first_off = tl.events.iter().find(|e| e.velocity == 0 && e.key == 45).unwrap();
        // Tie extends the first note through the second beat (2 x 0.5s).
        assert!((first_off.secs - 1.0).abs() < 1e-9);
    }

    #[test]
    fn closest_occurrence_picks_nearest_pass() {
        let score = score_with_bars(vec![
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { repeat_jumps: 1, ..bar() },
            bar(),
        ]);
        let tl = Timeline::build(&score);
        // Order: 0,1,0,1,2 -> bar 0 starts at 0s and 4s.
        assert_eq!(tl.closest_occurrence(0, 0.5), Some(0));
        assert_eq!(tl.closest_occurrence(0, 7.0), Some(2));
        assert_eq!(tl.closest_occurrence(5, 0.0), None);
    }

    #[test]
    fn linear_sync_round_trips() {
        let map = SyncMap::Linear { offset_secs: 1.5, tempo_ratio: 1.25 };
        let audio = map.score_to_audio(10.0);
        assert!((audio - 9.5).abs() < 1e-9);
        assert!((map.audio_to_score(audio) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn point_sync_interpolates_and_extrapolates() {
        let map = SyncMap::Points(vec![(0.0, 2.0), (10.0, 12.0), (20.0, 24.0)]);
        assert!((map.score_to_audio(5.0) - 7.0).abs() < 1e-9);
        assert!((map.score_to_audio(15.0) - 18.0).abs() < 1e-9);
        assert!((map.audio_to_score(18.0) - 15.0).abs() < 1e-9);
        // Beyond the last point the last segment's slope continues.
        assert!((map.score_to_audio(25.0) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn file_sync_points_resolve_bar_occurrences() {
        let score = score_with_bars(vec![
            MasterBar { repeat_open: true, ..bar() },
            MasterBar { repeat_jumps: 1, ..bar() },
        ]);
        let tl = Timeline::build(&score);
        let points = vec![
            FileSyncPoint { bar: 0, occurrence: 0, audio_secs: 1.0 },
            FileSyncPoint { bar: 0, occurrence: 1, audio_secs: 5.5 },
        ];
        let map = SyncMap::from_file_points(&points, &tl).unwrap();
        // Second pass of bar 0 starts at score 4.0s.
        assert_eq!(map, SyncMap::Points(vec![(0.0, 1.0), (4.0, 5.5)]));
    }
}
