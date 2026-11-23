//! Practice mode for the tab player: listens to the guitar and checks each
//! note of the selected track against what was actually played.
//!
//! ## How a note is judged
//!
//! The guitar signal (the same post-chain samples the tuner and oscilloscope
//! use) is kept in a short history, with every UI frame's batch of samples
//! tagged with the song position at that moment. For each beat of the tab,
//! once enough audio has arrived, a window starting at the beat's time (plus
//! the audio round-trip latency, plus a small search offset to forgive early
//! or late picking) is analysed:
//!
//! - For every expected pitch, Goertzel filters measure the power at its
//!   first harmonics. The share of the window's power they explain must
//!   reach a threshold that scales with [`PracticeSettings::strictness`]
//!   and with how many notes the beat has (a chord splits the power).
//! - The pitch must also explain more power than the semitones next to it
//!   (catches "one fret off") and, for single notes, than the pitches a
//!   fourth/fifth away, whose harmonics overlap with its own (catches
//!   "wrong string, same fret").
//!
//! A beat counts as correct when all of its notes are found (chords of 3+
//! notes allow one in four to be missing, since strummed chords often lose a
//! string). Octave errors are accepted (the harmonics overlap), and the same
//! pitch on another string is indistinguishable by ear, so it's accepted too.
//!
//! This runs on the UI thread, never on the audio callback.

use std::collections::{HashMap, VecDeque};

use crate::shared::tab::model::ScoreTrack;
use crate::shared::tab::timeline::Timeline;

/// Analysis window: 64 ms at 48 kHz. Long enough to separate semitones on
/// the low strings through their upper harmonics, short enough for 16ths.
const WINDOW: usize = 3072;
/// Highest harmonic considered, and the frequency ceiling for harmonics.
const MAX_HARMONICS: usize = 10;
const MAX_HARMONIC_HZ: f64 = 4000.0;
/// Step of the early/late search inside the timing tolerance.
const OFFSET_STEP_SECS: f64 = 0.02;
/// Guitar audio kept for analysis.
const HISTORY_SECS: f64 = 4.0;
/// Below this RMS (full scale = 1.0) the window counts as silence.
const SILENCE_RMS: f64 = 0.003;
/// A position change larger than this between two frames is a seek.
const SEEK_JUMP_SECS: f64 = 0.5;
/// Beats this close together (different voices) are judged as one.
const MERGE_SECS: f64 = 0.002;
/// Pitches (in semitones from the expected one) that share harmonics with
/// it and that the expected pitch must out-score: one fret off, and a
/// fourth/fifth away (e.g. a note a fourth up has its 3rd harmonic on the
/// expected note's 4th). Octaves are left out on purpose (accepted).
const CONFUSABLE_INTERVALS: [i16; 6] = [-7, -5, -1, 1, 5, 7];

/// Identifies one note on the tab: bar, voice, beat index in the voice and
/// note index in the beat (the indices used by `TrackBar::voices`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteKey {
    pub bar: usize,
    pub voice: u8,
    pub beat: u16,
    pub note: u8,
}

/// Notes that should sound together at `secs` (score time).
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectedBeat {
    pub secs: f64,
    /// `(where it is on the tab, sounding MIDI pitch)`.
    pub notes: Vec<(NoteKey, u8)>,
}

/// Lists the notes of `track` in playing order (repeats expanded). Skips
/// what can't or shouldn't be checked by pitch: percussion tracks, tied
/// notes (not re-picked), dead notes, ghost notes and harmonics.
pub fn expected_beats(track: &ScoreTrack, timeline: &Timeline) -> Vec<ExpectedBeat> {
    if track.percussion {
        return Vec::new();
    }
    let mut beats: Vec<ExpectedBeat> = Vec::new();
    for occ in &timeline.occurrences {
        let Some(bar) = track.bars.get(occ.bar) else {
            continue;
        };
        for (voice_index, voice) in bar.voices.iter().enumerate() {
            for (beat_index, beat) in voice.iter().enumerate() {
                let notes: Vec<(NoteKey, u8)> = beat
                    .notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| !n.tie && !n.dead && !n.ghost && !n.harmonic)
                    .filter_map(|(note_index, n)| {
                        let string = (n.string as usize).clamp(1, track.string_count());
                        let open = *track.tuning.get(string - 1)? as i32;
                        let pitch = open + track.capo + n.fret as i32;
                        (0..=127).contains(&pitch).then(|| {
                            let key = NoteKey {
                                bar: occ.bar,
                                voice: voice_index as u8,
                                beat: beat_index as u16,
                                note: note_index as u8,
                            };
                            (key, pitch as u8)
                        })
                    })
                    .collect();
                if !notes.is_empty() {
                    beats.push(ExpectedBeat {
                        secs: occ.tick_to_secs(beat.start),
                        notes,
                    });
                }
            }
        }
    }
    beats.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    let mut merged: Vec<ExpectedBeat> = Vec::with_capacity(beats.len());
    for beat in beats {
        match merged.last_mut() {
            Some(last) if beat.secs - last.secs < MERGE_SECS => last.notes.extend(beat.notes),
            _ => merged.push(beat),
        }
    }
    merged
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PracticeSettings {
    /// Audio round-trip latency: how much later the guitar reaches the app
    /// than the moment the song was heard. Score seconds.
    pub latency_secs: f64,
    /// How early or late a note may be picked and still count.
    pub tolerance_secs: f64,
    /// 0 = lenient, 1 = strict.
    pub strictness: f64,
}

impl Default for PracticeSettings {
    fn default() -> Self {
        Self {
            latency_secs: 0.0,
            tolerance_secs: 0.12,
            strictness: 0.5,
        }
    }
}

impl PracticeSettings {
    /// Share of the window's power a single note's harmonics must explain.
    fn min_fraction(&self, notes_in_beat: usize) -> f64 {
        let base = 0.05 + 0.30 * self.strictness.clamp(0.0, 1.0);
        base / notes_in_beat.max(1) as f64
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PracticeStats {
    pub notes_judged: u32,
    pub notes_hit: u32,
    pub beats_judged: u32,
    pub beats_hit: u32,
    /// Consecutive correct beats.
    pub streak: u32,
    pub best_streak: u32,
}

impl PracticeStats {
    pub fn note_accuracy(&self) -> Option<f64> {
        (self.notes_judged > 0).then(|| self.notes_hit as f64 / self.notes_judged as f64)
    }
}

fn midi_to_hz(pitch: u8) -> f64 {
    440.0 * 2f64.powf((pitch as f64 - 69.0) / 12.0)
}

/// `|X(f)|²` of `samples` at frequency `freq` (Goertzel).
fn goertzel_power(samples: &[f64], freq: f64, sample_rate: f64) -> f64 {
    let omega = std::f64::consts::TAU * freq / sample_rate;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0, 0.0);
    for &x in samples {
        let s = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

/// One Hann-windowed analysis frame.
struct Frame {
    windowed: Vec<f64>,
    window_sum: f64,
    /// Window-weighted mean power of the raw samples.
    power: f64,
    rms: f64,
}

impl Frame {
    fn new(samples: impl Iterator<Item = f32>, hann: &[f64]) -> Self {
        let mut windowed = Vec::with_capacity(hann.len());
        let mut weighted_power = 0.0;
        let mut plain_power = 0.0;
        for (x, &w) in samples.zip(hann) {
            let x = x as f64;
            windowed.push(x * w);
            weighted_power += w * x * x;
            plain_power += x * x;
        }
        let window_sum: f64 = hann.iter().sum();
        let n = windowed.len().max(1) as f64;
        Frame {
            windowed,
            window_sum,
            power: weighted_power / window_sum.max(f64::EPSILON),
            rms: (plain_power / n).sqrt(),
        }
    }

    /// Share of the frame's power explained by the harmonics of `pitch`.
    /// For a sinusoid of amplitude `a`, the Hann-windowed Goertzel magnitude
    /// is `a * sum(w) / 2`, so each harmonic contributes `a² / 2`.
    fn pitch_fraction(&self, pitch: u8, sample_rate: f64) -> f64 {
        if self.power <= 0.0 {
            return 0.0;
        }
        let f0 = midi_to_hz(pitch);
        let ceiling = MAX_HARMONIC_HZ.min(sample_rate * 0.45);
        let mut explained = 0.0;
        for h in 1..=MAX_HARMONICS {
            let f = f0 * h as f64;
            if f > ceiling {
                break;
            }
            let magnitude = goertzel_power(&self.windowed, f, sample_rate).sqrt();
            let amplitude = 2.0 * magnitude / self.window_sum;
            explained += amplitude * amplitude / 2.0;
        }
        explained / self.power
    }
}

/// Which of `pitches` the frame contains, with their power shares.
fn match_pitches(
    frame: &Frame,
    pitches: &[u8],
    settings: &PracticeSettings,
    sample_rate: f64,
) -> Vec<(bool, f64)> {
    if frame.rms < SILENCE_RMS {
        return vec![(false, 0.0); pitches.len()];
    }
    let mut cache: HashMap<u8, f64> = HashMap::new();
    let mut fraction = |p: u8| {
        *cache
            .entry(p)
            .or_insert_with(|| frame.pitch_fraction(p, sample_rate))
    };
    let min_fraction = settings.min_fraction(pitches.len());
    pitches
        .iter()
        .map(|&p| {
            let own = fraction(p);
            // In a chord, the chord's root (often not played, like the
            // implied root of a fourths voicing) explains the notes' shared
            // harmonics better than any single note does, so only "one fret
            // off" is checked there.
            let intervals: &[i16] = if pitches.len() == 1 {
                &CONFUSABLE_INTERVALS
            } else {
                &[-1, 1]
            };
            let beats_neighbors = intervals
                .iter()
                .filter_map(|&interval| {
                    u8::try_from(p as i16 + interval).ok().filter(|n| *n <= 127)
                })
                .filter(|n| !pitches.contains(n))
                .all(|n| own >= fraction(n));
            (own >= min_fraction && beats_neighbors, own)
        })
        .collect()
}

fn beat_is_correct(hits: usize, notes: usize) -> bool {
    if notes <= 2 {
        hits == notes
    } else {
        hits * 4 >= notes * 3
    }
}

/// Live practice state for one track of one tab.
pub struct PracticeSession {
    sample_rate: f64,
    hann: Vec<f64>,
    beats: Vec<ExpectedBeat>,
    /// Next beat to judge.
    next: usize,
    samples: VecDeque<f32>,
    /// Absolute index (samples fed since the history was last cleared) of
    /// `samples[0]`.
    first_index: u64,
    /// `(absolute index one past a fed batch, score time when it was fed)`.
    anchors: VecDeque<(u64, f64)>,
    marks: HashMap<NoteKey, bool>,
    stats: PracticeStats,
    pub settings: PracticeSettings,
}

impl PracticeSession {
    pub fn new(beats: Vec<ExpectedBeat>, sample_rate: f64, settings: PracticeSettings) -> Self {
        let hann = (0..WINDOW)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (WINDOW - 1) as f64).cos())
            .collect();
        Self {
            sample_rate: sample_rate.max(1.0),
            hann,
            beats,
            next: 0,
            samples: VecDeque::new(),
            first_index: 0,
            anchors: VecDeque::new(),
            marks: HashMap::new(),
            stats: PracticeStats::default(),
            settings,
        }
    }

    pub fn beat_count(&self) -> usize {
        self.beats.len()
    }

    /// Latest result for each judged note (`true` = played correctly).
    pub fn marks(&self) -> &HashMap<NoteKey, bool> {
        &self.marks
    }

    pub fn stats(&self) -> PracticeStats {
        self.stats
    }

    /// Clears results and statistics (keeps settings).
    pub fn reset(&mut self) {
        self.marks.clear();
        self.stats = PracticeStats::default();
        let resume_at = self.anchors.back().map(|a| a.1);
        self.clear_history();
        if let Some(secs) = resume_at {
            self.reposition(secs);
        }
    }

    fn clear_history(&mut self) {
        self.samples.clear();
        self.anchors.clear();
        self.first_index = 0;
    }

    /// Skips beats that can no longer be judged from `score_secs` on.
    fn reposition(&mut self, score_secs: f64) {
        let perf = score_secs - self.settings.latency_secs - self.settings.tolerance_secs;
        self.next = self.beats.partition_point(|b| b.secs < perf);
    }

    fn window_secs(&self) -> f64 {
        WINDOW as f64 / self.sample_rate
    }

    /// Feeds the guitar samples that arrived since the last call. `score_secs`
    /// is the song position now; `playing` whether the song is running.
    /// Judges every beat that has enough audio after it.
    pub fn feed(&mut self, samples: &[f32], score_secs: f64, playing: bool) {
        if !playing {
            self.clear_history();
            return;
        }
        let batch_secs = samples.len() as f64 / self.sample_rate;
        match self.anchors.back() {
            None => self.reposition(score_secs - batch_secs),
            Some(&(_, last)) => {
                let jumped = score_secs < last - 0.05
                    || score_secs > last + SEEK_JUMP_SECS + 2.0 * batch_secs;
                if jumped {
                    self.clear_history();
                    self.reposition(score_secs - batch_secs);
                }
            }
        }

        self.samples.extend(samples.iter().copied());
        let end = self.first_index + self.samples.len() as u64;
        // A batch that brought no samples carries no new timing information.
        if self.anchors.back().is_none_or(|a| a.0 < end) {
            self.anchors.push_back((end, score_secs));
        }
        let max_len = (HISTORY_SECS * self.sample_rate) as usize;
        if self.samples.len() > max_len {
            let drop = self.samples.len() - max_len;
            self.samples.drain(..drop);
            self.first_index += drop as u64;
        }
        while self.anchors.len() > 2 && self.anchors[1].0 <= self.first_index {
            self.anchors.pop_front();
        }

        self.judge_ready_beats();
    }

    /// Absolute sample index that was being fed at score time `secs`,
    /// interpolating between batch anchors (extrapolating at the ends).
    fn index_at(&self, secs: f64) -> Option<f64> {
        let last = *self.anchors.back()?;
        let segment = if self.anchors.len() == 1 {
            None
        } else {
            let idx = self
                .anchors
                .partition_point(|a| a.1 <= secs)
                .clamp(1, self.anchors.len() - 1);
            Some((self.anchors[idx - 1], self.anchors[idx]))
        };
        match segment {
            Some(((i0, t0), (i1, t1))) if t1 - t0 > 1e-6 => {
                let rate = (i1 as f64 - i0 as f64) / (t1 - t0);
                Some(i0 as f64 + (secs - t0) * rate)
            }
            _ => Some(last.0 as f64 + (secs - last.1) * self.sample_rate),
        }
    }

    fn judge_ready_beats(&mut self) {
        let Some(&(end_index, now)) = self.anchors.back() else {
            return;
        };
        let settings = self.settings;
        // Score time of the newest guitar sample, as the player heard it.
        let performed_until = now - settings.latency_secs;
        while let Some(beat) = self.beats.get(self.next) {
            if beat.secs + settings.tolerance_secs + self.window_secs() > performed_until {
                break;
            }
            let beat = beat.clone();
            self.next += 1;
            if let Some(results) = self.judge(&beat, end_index) {
                self.record(&beat, &results);
            }
        }
    }

    fn frame_at(&self, offset: usize) -> Frame {
        Frame::new(
            self.samples.range(offset..offset + WINDOW).copied(),
            &self.hann,
        )
    }

    /// Best match of `beat` over the allowed timing offsets, or `None` if
    /// the history doesn't cover it (e.g. right after pressing play).
    fn judge(&self, beat: &ExpectedBeat, end_index: u64) -> Option<Vec<bool>> {
        let pitches: Vec<u8> = beat.notes.iter().map(|(_, p)| *p).collect();
        let tolerance = self.settings.tolerance_secs.max(0.0);
        let steps = (tolerance / OFFSET_STEP_SECS).round() as i64;
        let min_fraction = self.settings.min_fraction(pitches.len());
        // Pick the offset where the expected pitches are strongest (cheap:
        // no neighbor checks), then run the full match only there.
        let mut best: Option<(usize, f64, usize)> = None;
        for step in -steps..=steps {
            let start_secs =
                beat.secs + self.settings.latency_secs + step as f64 * OFFSET_STEP_SECS;
            let Some(start) = self.index_at(start_secs) else {
                continue;
            };
            let start = start.round();
            if start < self.first_index as f64 || start + WINDOW as f64 > end_index as f64 {
                continue;
            }
            let offset = (start as u64 - self.first_index) as usize;
            let frame = self.frame_at(offset);
            let fractions: Vec<f64> = if frame.rms < SILENCE_RMS {
                vec![0.0; pitches.len()]
            } else {
                pitches
                    .iter()
                    .map(|&p| frame.pitch_fraction(p, self.sample_rate))
                    .collect()
            };
            let strong = fractions.iter().filter(|f| **f >= min_fraction).count();
            let score: f64 = fractions.iter().sum();
            if best.is_none_or(|(n, s, _)| strong > n || (strong == n && score > s)) {
                best = Some((strong, score, offset));
            }
        }
        let (_, _, offset) = best?;
        let matched = match_pitches(
            &self.frame_at(offset),
            &pitches,
            &self.settings,
            self.sample_rate,
        );
        Some(matched.into_iter().map(|m| m.0).collect())
    }

    fn record(&mut self, beat: &ExpectedBeat, results: &[bool]) {
        let hits = results.iter().filter(|h| **h).count();
        for ((key, _), &hit) in beat.notes.iter().zip(results) {
            self.marks.insert(*key, hit);
        }
        let stats = &mut self.stats;
        stats.notes_judged += results.len() as u32;
        stats.notes_hit += hits as u32;
        stats.beats_judged += 1;
        if beat_is_correct(hits, results.len()) {
            stats.beats_hit += 1;
            stats.streak += 1;
            stats.best_streak = stats.best_streak.max(stats.streak);
        } else {
            stats.streak = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::model::{MasterBar, Score, TabBeat, TabNote, TrackBar};

    const SR: f64 = 48_000.0;
    const STANDARD: [u8; 6] = [64, 59, 55, 50, 45, 40];

    /// A plucked-string-like tone: decaying harmonics with 1/h amplitudes.
    fn tone(pitches: &[u8], secs: f64) -> Vec<f32> {
        let n = (secs * SR) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / SR;
                let env = (-t * 2.0).exp();
                let mut sum = 0.0;
                for &p in pitches {
                    let f = midi_to_hz(p);
                    for h in 1..=6 {
                        sum += (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64;
                    }
                }
                (0.3 * env * sum / pitches.len() as f64) as f32
            })
            .collect()
    }

    fn frame_of(samples: &[f32]) -> Frame {
        let session = PracticeSession::new(Vec::new(), SR, PracticeSettings::default());
        Frame::new(samples[..WINDOW].iter().copied(), &session.hann)
    }

    fn matched(samples: &[f32], pitches: &[u8]) -> Vec<bool> {
        let frame = frame_of(samples);
        match_pitches(&frame, pitches, &PracticeSettings::default(), SR)
            .into_iter()
            .map(|m| m.0)
            .collect()
    }

    #[test]
    fn goertzel_measures_a_sine_amplitude() {
        let samples: Vec<f32> = (0..WINDOW)
            .map(|i| (0.5 * (std::f64::consts::TAU * 440.0 * i as f64 / SR).sin()) as f32)
            .collect();
        let frame = frame_of(&samples);
        // A pure 440 Hz sine is all A4: its fundamental explains ~all power.
        let fraction = frame.pitch_fraction(69, SR);
        assert!((0.9..1.1).contains(&fraction), "fraction {fraction}");
    }

    #[test]
    fn right_note_is_found_on_the_low_string() {
        // E2, the lowest standard-tuning note, is the hardest to resolve.
        assert_eq!(matched(&tone(&[40], 0.2), &[40]), vec![true]);
        assert_eq!(matched(&tone(&[45], 0.2), &[45]), vec![true]);
    }

    #[test]
    fn one_fret_off_is_rejected() {
        assert_eq!(matched(&tone(&[41], 0.2), &[40]), vec![false]);
        assert_eq!(matched(&tone(&[56], 0.2), &[57]), vec![false]);
    }

    #[test]
    fn unrelated_note_and_silence_are_rejected() {
        assert_eq!(matched(&tone(&[50], 0.2), &[45]), vec![false]);
        assert_eq!(matched(&vec![0.0; WINDOW], &[45]), vec![false]);
    }

    #[test]
    fn chord_notes_are_found_and_a_missing_one_is_flagged() {
        // E power chord: E2 B2 E3.
        assert_eq!(
            matched(&tone(&[40, 47, 52], 0.2), &[40, 47, 52]),
            vec![true, true, true]
        );
        // Stacked fourths (F3 Bb3 Eb4): their implied root Eb3 explains the
        // shared harmonics better than any played note, and mustn't matter.
        assert_eq!(
            matched(&tone(&[53, 58, 63], 0.2), &[53, 58, 63]),
            vec![true, true, true]
        );
        // A major (A2 E3 A3 C#4 E4) played without the C#.
        let played = tone(&[45, 52, 57, 64], 0.2);
        let result = matched(&played, &[45, 52, 57, 61, 64]);
        assert!(!result[3], "C#4 was not played: {result:?}");
    }

    #[test]
    fn beat_rule_allows_one_missing_string_in_big_chords() {
        assert!(beat_is_correct(1, 1));
        assert!(!beat_is_correct(1, 2));
        assert!(beat_is_correct(3, 4));
        assert!(!beat_is_correct(2, 4));
        assert!(beat_is_correct(5, 6));
    }

    fn one_bar_score(beats: Vec<TabBeat>) -> (Score, ScoreTrack) {
        let track = ScoreTrack {
            tuning: STANDARD.to_vec(),
            bars: vec![TrackBar {
                voices: vec![beats],
            }],
            ..Default::default()
        };
        let score = Score {
            initial_tempo: 60.0,
            bars: vec![MasterBar {
                numerator: 4,
                denominator: 4,
                ..Default::default()
            }],
            tracks: vec![track.clone()],
            ..Default::default()
        };
        (score, track)
    }

    fn note(string: u8, fret: i16) -> TabNote {
        TabNote {
            string,
            fret,
            velocity: 100,
            ..Default::default()
        }
    }

    #[test]
    fn expected_beats_use_tuning_and_capo_and_skip_unpickable_notes() {
        let beats = vec![
            TabBeat {
                start: 0,
                duration: 960,
                value: 4,
                notes: vec![note(6, 0), note(5, 2)],
                ..Default::default()
            },
            TabBeat {
                start: 960,
                duration: 960,
                value: 4,
                notes: vec![TabNote {
                    tie: true,
                    ..note(6, 0)
                }],
                ..Default::default()
            },
            TabBeat {
                start: 1920,
                duration: 960,
                value: 4,
                notes: vec![
                    TabNote {
                        dead: true,
                        ..note(1, 0)
                    },
                    note(1, 3),
                ],
                ..Default::default()
            },
        ];
        let (score, mut track) = one_bar_score(beats);
        track.capo = 2;
        let timeline = Timeline::build(&score);
        let expected = expected_beats(&track, &timeline);
        assert_eq!(expected.len(), 2, "tied-only beat is skipped");
        assert_eq!(expected[0].secs, 0.0);
        assert_eq!(
            expected[0].notes.iter().map(|n| n.1).collect::<Vec<_>>(),
            vec![42, 49]
        );
        assert!((expected[1].secs - 2.0).abs() < 1e-9);
        assert_eq!(expected[1].notes.len(), 1);
        assert_eq!(
            expected[1].notes[0].0,
            NoteKey {
                bar: 0,
                voice: 0,
                beat: 2,
                note: 1
            }
        );
        assert_eq!(expected[1].notes[0].1, 64 + 2 + 3);

        let percussion = ScoreTrack {
            percussion: true,
            ..track
        };
        assert!(expected_beats(&percussion, &timeline).is_empty());
    }

    /// Feeds `audio` to the session in UI-frame-sized batches, with the score
    /// clock running at normal speed and starting at `start_secs`.
    fn play(session: &mut PracticeSession, audio: &[f32], start_secs: f64) {
        let batch = 800; // one 60 fps frame at 48 kHz
        let mut fed = 0;
        for chunk in audio.chunks(batch) {
            fed += chunk.len();
            session.feed(chunk, start_secs + fed as f64 / SR, true);
        }
    }

    #[test]
    fn session_marks_right_and_wrong_notes_in_time() {
        // Quarter notes at 60 bpm: A2 at 0 s, C3 at 1 s.
        let beats = vec![
            TabBeat {
                start: 0,
                duration: 960,
                value: 4,
                notes: vec![note(5, 0)],
                ..Default::default()
            },
            TabBeat {
                start: 960,
                duration: 960,
                value: 4,
                notes: vec![note(5, 3)],
                ..Default::default()
            },
        ];
        let (score, track) = one_bar_score(beats);
        let timeline = Timeline::build(&score);
        let mut session = PracticeSession::new(
            expected_beats(&track, &timeline),
            SR,
            PracticeSettings::default(),
        );

        // Correct A2 (picked 30 ms late), then D3 instead of C3.
        let mut audio = vec![0.0f32; (0.03 * SR) as usize];
        audio.extend(tone(&[45], 0.97));
        audio.extend(tone(&[50], 1.0));
        play(&mut session, &audio, 0.0);

        let stats = session.stats();
        assert_eq!(stats.beats_judged, 2, "{stats:?}");
        assert_eq!(stats.notes_hit, 1);
        assert!(
            session.marks()[&NoteKey {
                bar: 0,
                voice: 0,
                beat: 0,
                note: 0
            }]
        );
        assert!(
            !session.marks()[&NoteKey {
                bar: 0,
                voice: 0,
                beat: 1,
                note: 0
            }]
        );
        assert_eq!(stats.streak, 0);
        assert_eq!(stats.best_streak, 1);
    }

    #[test]
    fn latency_compensation_shifts_the_analysis_window() {
        let beats = vec![TabBeat {
            start: 960,
            duration: 960,
            value: 4,
            notes: vec![note(5, 0)],
            ..Default::default()
        }];
        let (score, track) = one_bar_score(beats);
        let timeline = Timeline::build(&score);
        let settings = PracticeSettings {
            latency_secs: 0.3,
            tolerance_secs: 0.05,
            ..Default::default()
        };
        let mut session = PracticeSession::new(expected_beats(&track, &timeline), SR, settings);
        // The note reaches the app 300 ms after the beat (1.0 s).
        let mut audio = vec![0.0f32; (1.3 * SR) as usize];
        audio.extend(tone(&[45], 0.7));
        play(&mut session, &audio, 0.0);
        assert_eq!(session.stats().notes_hit, 1, "{:?}", session.stats());
    }

    #[test]
    fn pausing_and_seeking_skip_beats_without_judging_them() {
        let beats = (0..4)
            .map(|i| TabBeat {
                start: i * 960,
                duration: 960,
                value: 4,
                notes: vec![note(5, 0)],
                ..Default::default()
            })
            .collect();
        let (score, track) = one_bar_score(beats);
        let timeline = Timeline::build(&score);
        let mut session = PracticeSession::new(
            expected_beats(&track, &timeline),
            SR,
            PracticeSettings::default(),
        );

        // Start playing at 2.5 s: beats at 0, 1 and 2 s are never judged.
        play(&mut session, &tone(&[45], 1.5), 2.5);
        assert_eq!(session.stats().beats_judged, 1);
        assert_eq!(session.stats().beats_hit, 1);

        // Paused, then a seek back to 0: history restarts there.
        session.feed(&[0.0; 800], 1.0, false);
        play(&mut session, &tone(&[45], 0.5), 0.0);
        assert_eq!(session.stats().beats_judged, 2);

        session.reset();
        assert_eq!(session.stats(), PracticeStats::default());
        assert!(session.marks().is_empty());
    }
}
