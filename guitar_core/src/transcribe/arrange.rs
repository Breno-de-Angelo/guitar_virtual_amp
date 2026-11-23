//! Turns transcribed notes into a tab: groups notes struck together into
//! chords, picks a subdivision for every beat (16ths or 8th-note triplets,
//! by Viterbi over snapping error with a prior for straight time) and snaps
//! the chords onto it, chooses a string and fret for every note (Viterbi
//! over fingerings: stretch, hand moves, and a string prior learned from
//! GuitarSet players), and lays everything out as notated bars.

use super::basic_pitch::NoteEstimate;
use crate::shared::tab::model::{
    MasterBar, Score, ScoreTrack, TICKS_PER_QUARTER, TabBeat, TabNote, TrackBar,
};

/// Widest fretted-note stretch inside one chord.
const MAX_SPAN: i16 = 4;
const MAX_CANDIDATES: usize = 48;

#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentSpec {
    pub name: String,
    /// Open-string pitches, string 1 (highest) first.
    pub tuning: Vec<u8>,
    pub tuning_name: String,
    pub max_fret: i16,
    /// Most notes kept per chord.
    pub max_notes: usize,
    pub program: u8,
    /// Use the string prior learned from guitarists ([`StringPrior`]).
    pub fingering_prior: bool,
}

/// Low-end percentile of the pitches, ignoring a few stray low notes.
fn low_pitch(notes: &[NoteEstimate]) -> Option<u8> {
    let mut pitches: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
    if pitches.is_empty() {
        return None;
    }
    pitches.sort_unstable();
    Some(pitches[(pitches.len() as f64 * 0.02) as usize])
}

/// A guitar tuning that reaches the transcription's lowest notes.
pub fn guitar_spec(notes: &[NoteEstimate]) -> InstrumentSpec {
    let (tuning_name, tuning): (&str, Vec<u8>) = match low_pitch(notes).unwrap_or(40) {
        40.. => ("E padrão", vec![64, 59, 55, 50, 45, 40]),
        39 => ("Eb (meio tom abaixo)", vec![63, 58, 54, 49, 44, 39]),
        38 => ("Drop D", vec![64, 59, 55, 50, 45, 38]),
        37 => ("C# padrão", vec![61, 56, 52, 47, 42, 37]),
        36 => ("Drop C", vec![62, 57, 53, 48, 43, 36]),
        35 => ("7 cordas (B)", vec![64, 59, 55, 50, 45, 40, 35]),
        _ => ("8 cordas (F#)", vec![64, 59, 55, 50, 45, 40, 35, 30]),
    };
    InstrumentSpec {
        name: "Guitarra (IA)".into(),
        tuning,
        tuning_name: tuning_name.into(),
        max_fret: 22,
        max_notes: 6,
        program: 29,
        fingering_prior: true,
    }
}

pub fn bass_spec(notes: &[NoteEstimate]) -> InstrumentSpec {
    let five_string = low_pitch(notes).is_some_and(|p| p < 28);
    let (tuning_name, tuning) = if five_string {
        ("5 cordas (B E A D G)", vec![43, 38, 33, 28, 23])
    } else {
        ("E padrão", vec![43, 38, 33, 28])
    };
    InstrumentSpec {
        name: "Baixo (IA)".into(),
        tuning,
        tuning_name: tuning_name.into(),
        max_fret: 20,
        max_notes: 2,
        program: 33,
        fingering_prior: false,
    }
}

/// Beat times of the tab, index 0 being the first downbeat: `bars *
/// beats_per_bar + 1` entries (the last one closes the last bar).
#[derive(Clone, Debug, PartialEq)]
pub struct BeatGrid {
    pub times: Vec<f64>,
    pub beats_per_bar: usize,
}

impl BeatGrid {
    pub fn beats(&self) -> usize {
        self.times.len().saturating_sub(1)
    }

    pub fn bars(&self) -> usize {
        self.beats() / self.beats_per_bar.max(1)
    }

    pub fn bar_ticks(&self) -> i64 {
        self.beats_per_bar as i64 * TICKS_PER_BEAT
    }

    pub fn total_ticks(&self) -> i64 {
        self.bars() as i64 * self.bar_ticks()
    }

    /// Audio time of every bar start (plus the end of the last bar).
    pub fn bar_starts(&self) -> Vec<f64> {
        self.times
            .iter()
            .step_by(self.beats_per_bar.max(1))
            .copied()
            .collect()
    }

    /// Fractional beat index of time `t` (linear within each beat,
    /// extrapolated with the first/last beat outside the grid).
    pub fn beat_position(&self, t: f64) -> f64 {
        let g = &self.times;
        let n = g.len();
        if n < 2 {
            return 0.0;
        }
        let b = g.partition_point(|x| *x <= t).clamp(1, n - 1) - 1;
        let span = (g[b + 1] - g[b]).max(1e-6);
        b as f64 + (t - g[b]) / span
    }
}

/// Beats are quarter notes.
pub const TICKS_PER_BEAT: i64 = TICKS_PER_QUARTER;
/// Subdivisions a beat can be quantized to: 16th notes or 8th-note triplets.
pub const STRAIGHT: usize = 4;
pub const TRIPLET: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantizeParams {
    /// Onsets this close to the first one of a group are one chord
    /// (strums spread the strings over a few tens of ms).
    pub chord_secs: f64,
    /// Expected timing spread of played notes, in beats.
    pub jitter_beats: f64,
    /// Cost of notating a beat as a triplet (a prior for straight time).
    pub triplet_cost: f64,
    /// Cost of switching between straight and triplet beats.
    pub switch_cost: f64,
}

impl Default for QuantizeParams {
    fn default() -> Self {
        Self {
            chord_secs: 0.035,
            jitter_beats: 0.05,
            triplet_cost: 3.5,
            switch_cost: 1.5,
        }
    }
}

/// Notes starting together on the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct GridEvent {
    /// Start, in ticks from the first bar.
    pub tick: i64,
    /// Tick where the longest note of the event ends.
    pub end_tick: i64,
    /// Audio time of the onset.
    pub secs: f64,
    /// `(pitch, amplitude)`, loudest first.
    pub pitches: Vec<(u8, f32)>,
}

/// Quantized events plus the subdivision chosen for every beat.
#[derive(Clone, Debug, PartialEq)]
pub struct Quantized {
    pub events: Vec<GridEvent>,
    /// [`STRAIGHT`] or [`TRIPLET`], one per beat of the grid.
    pub subdivisions: Vec<usize>,
}

/// Beat owning a fractional position: onsets up to half a 16th early still
/// belong to the beat they anticipate.
fn owning_beat(pos: f64, beats: usize) -> usize {
    ((pos + 0.125).floor().max(0.0) as usize).min(beats.saturating_sub(1))
}

/// Tick of a fractional beat position on beat `b` with `d` subdivisions.
fn snap(pos: f64, b: usize, d: usize) -> i64 {
    let k = ((pos - b as f64) * d as f64).round().clamp(0.0, d as f64) as i64;
    b as i64 * TICKS_PER_BEAT + k * TICKS_PER_BEAT / d as i64
}

/// Squared snapping error of `pos` to `d` subdivisions, in jitter units
/// (capped so one stray note can't force a grid).
fn snap_cost(pos: f64, b: usize, d: usize, jitter: f64) -> f64 {
    let f = (pos - b as f64) * d as f64;
    let err = (f - f.round()) / d as f64;
    (err / jitter).powi(2).min(9.0)
}

/// Picks straight 16ths or 8th triplets for every beat (Viterbi over beats:
/// snapping error, a prior for straight beats and a cost for switching),
/// then snaps onsets and ends to the chosen grid.
pub fn choose_subdivisions(positions: &[f64], beats: usize, params: &QuantizeParams) -> Vec<usize> {
    const OPTIONS: [usize; 2] = [STRAIGHT, TRIPLET];
    if beats == 0 {
        return Vec::new();
    }
    let mut local = vec![[0.0f64; 2]; beats];
    for &pos in positions {
        let b = owning_beat(pos, beats);
        for (k, &d) in OPTIONS.iter().enumerate() {
            local[b][k] += snap_cost(pos, b, d, params.jitter_beats);
        }
    }
    for l in &mut local {
        l[1] += params.triplet_cost;
    }
    let mut cost = local[0];
    let mut back = vec![[0usize; 2]; beats];
    for b in 1..beats {
        let mut next = [0.0; 2];
        for k in 0..2 {
            let (j, c) = (0..2)
                .map(|j| {
                    let switch = if j == k { 0.0 } else { params.switch_cost };
                    (j, cost[j] + switch)
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            next[k] = c + local[b][k];
            back[b][k] = j;
        }
        cost = next;
    }
    let mut k = if cost[0] <= cost[1] { 0 } else { 1 };
    let mut out = vec![STRAIGHT; beats];
    for b in (0..beats).rev() {
        out[b] = OPTIONS[k];
        k = back[b][k];
    }
    out
}

/// Groups notes into chords (onsets within `chord_secs`), picks a
/// subdivision for every beat and snaps the chords onto it.
pub fn quantize(
    notes: &[NoteEstimate],
    grid: &BeatGrid,
    max_notes: usize,
    params: &QuantizeParams,
) -> Quantized {
    let beats = grid.beats();
    let total = grid.total_ticks();
    if beats == 0 || notes.is_empty() {
        return Quantized {
            events: Vec::new(),
            subdivisions: vec![STRAIGHT; beats],
        };
    }
    let mut sorted: Vec<&NoteEstimate> = notes.iter().collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Chords: (onset time, notes).
    let mut chords: Vec<(f64, Vec<&NoteEstimate>)> = Vec::new();
    for note in sorted {
        match chords.last_mut() {
            Some((t, members)) if note.start - *t <= params.chord_secs => members.push(note),
            _ => chords.push((note.start, vec![note])),
        }
    }
    let positions: Vec<f64> = chords.iter().map(|(t, _)| grid.beat_position(*t)).collect();
    let subdivisions = choose_subdivisions(&positions, beats, params);
    let mut events: Vec<GridEvent> = Vec::new();
    for ((t, members), &pos) in chords.iter().zip(&positions) {
        let b = owning_beat(pos, beats);
        let d = subdivisions[b];
        let tick = snap(pos, b, d).clamp(0, total - TICKS_PER_BEAT / d as i64);
        let min_len = TICKS_PER_BEAT / d as i64;
        let end_tick = members
            .iter()
            .map(|n| {
                let end_pos = grid.beat_position(n.end);
                let eb = owning_beat(end_pos, beats);
                if end_pos >= beats as f64 {
                    total
                } else {
                    snap(end_pos, eb, subdivisions[eb])
                }
            })
            .max()
            .unwrap_or(tick)
            .clamp(tick + min_len, total.max(tick + min_len));
        let index = match events.iter().position(|e| e.tick == tick) {
            Some(i) => i,
            None => {
                events.push(GridEvent {
                    tick,
                    end_tick,
                    secs: *t,
                    pitches: Vec::new(),
                });
                events.len() - 1
            }
        };
        let event = &mut events[index];
        event.end_tick = event.end_tick.max(end_tick);
        for n in members {
            match event.pitches.iter_mut().find(|(p, _)| *p == n.pitch) {
                Some(existing) => existing.1 = existing.1.max(n.amplitude),
                None => event.pitches.push((n.pitch, n.amplitude)),
            }
        }
    }
    for event in &mut events {
        event.pitches.sort_by(|a, b| b.1.total_cmp(&a.1));
        event.pitches.truncate(max_notes.max(1));
    }
    events.sort_by_key(|e| e.tick);
    Quantized {
        events,
        subdivisions,
    }
}

/// `(string, fret)` per note of a chord, same order as its pitches.
type Fingering = Vec<(u8, i16)>;
/// A placed note: `(pitch, string, fret, amplitude)`.
pub type FrettedNote = (u8, u8, i16, f32);

/// Weights of the fingering cost (a hand-position HMM in the spirit of
/// Sayegh's optimum path and Hori & Sagayama's left-hand-form HMM). Fitted
/// on GuitarSet players 00-02 and human Guitar Pro tabs with
/// `eval_fingering` (`GVA_FIT_FINGERING=1`); see `ROADMAP.md` for the
/// numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FingeringCosts {
    /// Per fret between the lowest and highest fretted note of a chord.
    pub span: f64,
    /// Per fret of hand position (lowest fretted note): favors low positions.
    pub position: f64,
    /// Per fret above the 12th.
    pub high: f64,
    /// Per open string (negative: open strings are welcome).
    pub open: f64,
    /// Per unused string inside a chord (it has to be muted).
    pub gap: f64,
    /// Per fret of hand movement between events.
    pub moving: f64,
    /// Weight of the learned prior `-ln P(string | pitch)` ([`StringPrior`]).
    pub prior: f64,
}

impl Default for FingeringCosts {
    fn default() -> Self {
        Self {
            span: 0.4,
            position: -0.01,
            high: 0.08,
            open: 0.06,
            gap: 0.8,
            moving: 0.28,
            prior: 0.11,
        }
    }
}

/// Notes per (string, fret) played by GuitarSet players 00-02 (34150 notes;
/// GuitarSet, Xi et al. 2018, CC BY 4.0). Row 0 = string 1 (high e).
const GUITARSET_STRING_FRET_COUNTS: [[u16; 25]; 6] = [
    [
        268, 252, 256, 259, 396, 282, 337, 178, 350, 190, 164, 76, 71, 33, 21, 6, 11, 1, 0, 0, 0,
        0, 0, 0, 0,
    ],
    [
        366, 398, 480, 562, 541, 417, 714, 438, 601, 426, 424, 285, 137, 161, 36, 36, 20, 2, 0, 2,
        0, 0, 0, 0, 0,
    ],
    [
        554, 385, 726, 635, 693, 635, 1128, 782, 866, 646, 563, 314, 124, 99, 14, 30, 0, 0, 0, 0,
        0, 0, 0, 0, 0,
    ],
    [
        468, 80, 644, 976, 756, 720, 1233, 665, 1076, 478, 520, 269, 111, 70, 6, 1, 0, 1, 2, 0, 0,
        0, 0, 0, 0,
    ],
    [
        493, 265, 600, 730, 990, 410, 1040, 363, 531, 214, 152, 110, 62, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0,
    ],
    [
        370, 357, 280, 399, 463, 158, 332, 87, 99, 98, 71, 2, 6, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
];

/// How often players use each (string, fret), as a prior over the strings
/// a pitch can be played on. Strings without data (7th, 8th) get the
/// smoothing count only.
#[derive(Clone, Debug, PartialEq)]
pub struct StringPrior {
    /// `counts[string - 1][fret]`.
    pub counts: Vec<[f64; 25]>,
}

impl StringPrior {
    /// Added to every count, so unusual positions stay possible.
    const SMOOTHING: f64 = 20.0;

    pub fn guitarset() -> &'static StringPrior {
        static PRIOR: std::sync::OnceLock<StringPrior> = std::sync::OnceLock::new();
        PRIOR.get_or_init(|| StringPrior {
            counts: GUITARSET_STRING_FRET_COUNTS
                .iter()
                .map(|row| std::array::from_fn(|f| row[f] as f64))
                .collect(),
        })
    }

    fn count(&self, string: u8, fret: i16) -> f64 {
        let c = self
            .counts
            .get(string as usize - 1)
            .and_then(|row| row.get(fret.max(0) as usize))
            .copied()
            .unwrap_or(0.0);
        c + Self::SMOOTHING
    }

    /// `-ln P(string | pitch)` among the strings that can play `pitch`.
    fn cost(&self, pitch: u8, string: u8, tuning: &[u8], max_fret: i16) -> f64 {
        let total: f64 = tuning
            .iter()
            .enumerate()
            .filter_map(|(s, &open)| {
                let fret = pitch as i16 - open as i16;
                (0..=max_fret)
                    .contains(&fret)
                    .then(|| self.count(s as u8 + 1, fret))
            })
            .sum();
        let fret = pitch as i16 - tuning[string as usize - 1] as i16;
        -(self.count(string, fret) / total.max(1e-9)).ln()
    }
}

fn fretted_frets(f: &Fingering) -> impl Iterator<Item = i16> + '_ {
    f.iter().map(|n| n.1).filter(|&fr| fr > 0)
}

/// Hand position: the lowest fretted note (`None` for open strings only).
fn hand_position(f: &Fingering) -> Option<i16> {
    fretted_frets(f).min()
}

fn static_cost(f: &Fingering, c: &FingeringCosts) -> f64 {
    let lo = fretted_frets(f).min();
    let hi = fretted_frets(f).max();
    let span = match (lo, hi) {
        (Some(lo), Some(hi)) => (hi - lo) as f64,
        _ => 0.0,
    };
    let high: f64 = f.iter().map(|n| (n.1 - 12).max(0) as f64).sum();
    let opens = f.iter().filter(|n| n.1 == 0).count() as f64;
    let gaps = if f.len() >= 2 {
        let top = f.iter().map(|n| n.0).min().unwrap_or(1);
        let bottom = f.iter().map(|n| n.0).max().unwrap_or(1);
        (bottom - top + 1) as f64 - f.len() as f64
    } else {
        0.0
    };
    span * c.span
        + lo.unwrap_or(0) as f64 * c.position
        + high * c.high
        + opens * c.open
        + gaps * c.gap
}

/// Cost of moving the hand from `from` (where it was last fretting) to
/// play `cur`. Open strings don't move the hand.
fn transition_cost(from: Option<i16>, cur: &Fingering, c: &FingeringCosts) -> f64 {
    match (from, hand_position(cur)) {
        (Some(a), Some(b)) => (a - b).abs() as f64 * c.moving,
        _ => 0.0,
    }
}

/// Fretting hand feasibility: at most four fingers, unless the notes on the
/// lowest fret are barred by the index finger.
fn fingers_ok(f: &Fingering) -> bool {
    let fretted: Vec<i16> = fretted_frets(f).collect();
    if fretted.len() <= 4 {
        return true;
    }
    let lo = *fretted.iter().min().unwrap();
    fretted.iter().filter(|&&fr| fr > lo).count() <= 3
}

/// Every playable way to put `pitches` on distinct strings.
fn candidates(pitches: &[u8], tuning: &[u8], max_fret: i16) -> Vec<Fingering> {
    fn search(
        pitches: &[u8],
        tuning: &[u8],
        max_fret: i16,
        current: &mut Fingering,
        out: &mut Vec<Fingering>,
    ) {
        if out.len() >= 4000 {
            return;
        }
        let Some((&pitch, rest)) = pitches.split_first() else {
            if fingers_ok(current) {
                out.push(current.clone());
            }
            return;
        };
        for (s, &open) in tuning.iter().enumerate() {
            let string = (s + 1) as u8;
            let fret = pitch as i16 - open as i16;
            if !(0..=max_fret).contains(&fret) || current.iter().any(|n| n.0 == string) {
                continue;
            }
            current.push((string, fret));
            let ok = match (fretted_frets(current).min(), fretted_frets(current).max()) {
                (Some(lo), Some(hi)) => hi - lo <= MAX_SPAN,
                _ => true,
            };
            if ok {
                search(rest, tuning, max_fret, current, out);
            }
            current.pop();
        }
    }
    let mut out = Vec::new();
    search(pitches, tuning, max_fret, &mut Vec::new(), &mut out);
    out
}

/// Picks a fingering for every event, minimizing stretch and hand moves.
/// Notes that can't be played with the others (out of range, or no string
/// left within reach) are dropped, quietest first. Returns, per event, the
/// kept `(pitch, string, fret, amplitude)`.
pub fn fret_events(events: &[GridEvent], spec: &InstrumentSpec) -> Vec<Vec<FrettedNote>> {
    let prior = spec.fingering_prior.then(StringPrior::guitarset);
    fret_events_with(events, spec, &FingeringCosts::default(), prior)
}

pub fn fret_events_with(
    events: &[GridEvent],
    spec: &InstrumentSpec,
    costs: &FingeringCosts,
    prior: Option<&StringPrior>,
) -> Vec<Vec<FrettedNote>> {
    let event_cost = |f: &Fingering, pitches: &[(u8, f32)]| {
        let learned = match prior {
            Some(prior) if costs.prior != 0.0 => {
                costs.prior
                    * pitches
                        .iter()
                        .zip(f)
                        .map(|((p, _), (s, _))| prior.cost(*p, *s, &spec.tuning, spec.max_fret))
                        .sum::<f64>()
            }
            _ => 0.0,
        };
        static_cost(f, costs) + learned
    };
    // Candidate fingerings per event (with the pitches they cover).
    type Options = (Vec<(u8, f32)>, Vec<Fingering>);
    let mut options: Vec<Options> = Vec::with_capacity(events.len());
    for event in events {
        let mut pitches = event.pitches.clone();
        loop {
            let ps: Vec<u8> = pitches.iter().map(|p| p.0).collect();
            let mut found = candidates(&ps, &spec.tuning, spec.max_fret);
            if !found.is_empty() || pitches.len() <= 1 {
                found.sort_by(|a, b| event_cost(a, &pitches).total_cmp(&event_cost(b, &pitches)));
                found.truncate(MAX_CANDIDATES);
                options.push((pitches, found));
                break;
            }
            pitches.pop(); // quietest
        }
    }

    // Viterbi over fingerings. Each state also carries the hand position
    // of its best path, so open strings in between don't reset it.
    let n = options.len();
    let mut cost: Vec<Vec<f64>> = Vec::with_capacity(n);
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(n);
    let mut carried: Vec<Vec<Option<i16>>> = Vec::with_capacity(n);
    for i in 0..n {
        let current = &options[i].1;
        let mut row_cost = Vec::with_capacity(current.len());
        let mut row_back = Vec::with_capacity(current.len());
        let mut row_hand = Vec::with_capacity(current.len());
        for cand in current {
            let base = event_cost(cand, &options[i].0);
            if i == 0 || options[i - 1].1.is_empty() {
                row_cost.push(base);
                row_back.push(usize::MAX);
                row_hand.push(hand_position(cand));
                continue;
            }
            let (best_j, best) = (0..options[i - 1].1.len())
                .map(|j| {
                    (
                        j,
                        cost[i - 1][j] + transition_cost(carried[i - 1][j], cand, costs),
                    )
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((usize::MAX, 0.0));
            row_cost.push(base + best);
            row_back.push(best_j);
            row_hand.push(hand_position(cand).or(carried[i - 1].get(best_j).copied().flatten()));
        }
        cost.push(row_cost);
        back.push(row_back);
        carried.push(row_hand);
    }
    let mut chosen = vec![usize::MAX; n];
    // Walk back from the end; an event without options restarts the chain.
    let mut i = n;
    while i > 0 {
        i -= 1;
        if options[i].1.is_empty() {
            continue;
        }
        if chosen[i] == usize::MAX {
            chosen[i] = (0..cost[i].len())
                .min_by(|a, b| cost[i][*a].total_cmp(&cost[i][*b]))
                .unwrap_or(0);
        }
        let prev = back[i][chosen[i]];
        if i > 0 && prev != usize::MAX && !options[i - 1].1.is_empty() {
            chosen[i - 1] = prev;
        }
    }
    options
        .iter()
        .zip(chosen)
        .map(|((pitches, cands), c)| match cands.get(c) {
            Some(f) => pitches
                .iter()
                .zip(f)
                .map(|((p, a), (s, fr))| (*p, *s, *fr, *a))
                .collect(),
            None => Vec::new(),
        })
        .collect()
}

/// Splits a length in 16ths into notated values `(value, dotted, steps)`.
pub fn split_steps(mut steps: usize) -> Vec<(u16, bool, usize)> {
    const VALUES: [(usize, u16, bool); 8] = [
        (16, 1, false),
        (12, 2, true),
        (8, 2, false),
        (6, 4, true),
        (4, 4, false),
        (3, 8, true),
        (2, 8, false),
        (1, 16, false),
    ];
    let mut out = Vec::new();
    while steps > 0 {
        let &(len, value, dotted) = VALUES
            .iter()
            .find(|(len, _, _)| *len <= steps)
            .expect("1 always fits");
        out.push((value, dotted, len));
        steps -= len;
    }
    out
}

/// One notated piece: `(start tick, ticks, value, dotted, tuplet)`.
pub type Piece = (i64, i64, u16, bool, Option<u8>);

/// Notated pieces covering `start..start + len` (absolute ticks). Beats
/// notated as triplets get triplet 8ths/quarters (a whole triplet beat is
/// just a quarter); straight stretches get the usual dotted/plain values.
pub fn notate(start: i64, len: i64, subdivisions: &[usize]) -> Vec<Piece> {
    let end = start + len;
    let triplet_at = |beat: i64| {
        usize::try_from(beat)
            .ok()
            .and_then(|b| subdivisions.get(b))
            .is_some_and(|d| *d == TRIPLET)
    };
    let unit3 = TICKS_PER_BEAT / TRIPLET as i64;
    let unit4 = TICKS_PER_BEAT / STRAIGHT as i64;
    let mut out = Vec::new();
    let mut t = start;
    while t < end {
        let beat = t.div_euclid(TICKS_PER_BEAT);
        let beat_start = beat * TICKS_PER_BEAT;
        let beat_end = beat_start + TICKS_PER_BEAT;
        if triplet_at(beat) && !(t == beat_start && end >= beat_end) {
            // Part of a triplet beat.
            let piece_end = end.min(beat_end);
            while t + unit3 <= piece_end {
                if t + 2 * unit3 <= piece_end && t - beat_start <= unit3 {
                    out.push((t, 2 * unit3, 4, false, Some(3)));
                    t += 2 * unit3;
                } else {
                    out.push((t, unit3, 8, false, Some(3)));
                    t += unit3;
                }
            }
            if t < piece_end {
                // Off the triplet grid (shouldn't happen): one 8th triplet.
                out.push((t, piece_end - t, 8, false, Some(3)));
                t = piece_end;
            }
            continue;
        }
        // A straight stretch: up to the end, or to a partly used triplet beat.
        let mut run_end = t;
        while run_end < end {
            let b = run_end.div_euclid(TICKS_PER_BEAT);
            let bs = b * TICKS_PER_BEAT;
            if triplet_at(b) {
                if run_end == bs && end >= bs + TICKS_PER_BEAT {
                    run_end = bs + TICKS_PER_BEAT;
                } else {
                    break;
                }
            } else {
                run_end = end.min(bs + TICKS_PER_BEAT);
            }
        }
        let steps = ((run_end - t) / unit4).max(1) as usize;
        let mut at = t;
        for (value, dotted, n) in split_steps(steps) {
            let ticks = n as i64 * unit4;
            out.push((at, ticks, value, dotted, None));
            at += ticks;
        }
        t = run_end.max(at);
    }
    out
}

fn push_rest(
    beats: &mut Vec<TabBeat>,
    bar_start: i64,
    start: i64,
    len: i64,
    subdivisions: &[usize],
) {
    for (at, ticks, value, dotted, tuplet) in notate(start, len, subdivisions) {
        beats.push(TabBeat {
            start: at - bar_start,
            duration: ticks,
            value,
            dotted,
            tuplet,
            ..Default::default()
        });
    }
}

/// Lays fretted events out as bars of `grid.beats_per_bar` quarter notes.
pub fn build_track(
    spec: &InstrumentSpec,
    quantized: &Quantized,
    frets: &[Vec<FrettedNote>],
    grid: &BeatGrid,
) -> ScoreTrack {
    let bars = grid.bars();
    let bar_ticks = grid.bar_ticks();
    let subs = &quantized.subdivisions;
    let mut track_bars = vec![
        TrackBar {
            voices: vec![Vec::new()]
        };
        bars
    ];
    let playable: Vec<(i64, i64, &Vec<FrettedNote>)> = quantized
        .events
        .iter()
        .zip(frets)
        .filter(|(_, f)| !f.is_empty())
        .map(|(e, f)| (e.tick, e.end_tick, f))
        .collect();
    for (bar_index, bar) in track_bars.iter_mut().enumerate() {
        let bar_start = bar_index as i64 * bar_ticks;
        let bar_end = bar_start + bar_ticks;
        let beats = &mut bar.voices[0];
        let in_bar: Vec<_> = playable
            .iter()
            .filter(|(s, _, _)| (bar_start..bar_end).contains(s))
            .collect();
        let mut cursor = bar_start;
        for (k, (tick, end_tick, notes)) in in_bar.iter().enumerate() {
            if *tick > cursor {
                push_rest(beats, bar_start, cursor, tick - cursor, subs);
            }
            let next = in_bar.get(k + 1).map(|n| n.0).unwrap_or(bar_end);
            let gap = next - tick;
            let sounding = (end_tick - tick).max(1);
            // Short notes followed by a long gap get a rest after them.
            let note_len = if gap > TICKS_PER_BEAT && sounding + TICKS_PER_BEAT / 2 <= gap {
                sounding
            } else {
                gap
            };
            let tab_notes: Vec<TabNote> = notes
                .iter()
                .map(|(_, string, fret, amp)| TabNote {
                    string: *string,
                    fret: *fret,
                    velocity: (60.0 + 67.0 * amp.clamp(0.0, 1.0)) as u8,
                    ..Default::default()
                })
                .collect();
            let pieces = notate(*tick, note_len, subs);
            let notated_end = pieces.last().map(|p| p.0 + p.1).unwrap_or(*tick);
            for (i, (at, ticks, value, dotted, tuplet)) in pieces.into_iter().enumerate() {
                let notes = tab_notes
                    .iter()
                    .map(|n| TabNote {
                        tie: i > 0,
                        ..n.clone()
                    })
                    .collect();
                beats.push(TabBeat {
                    start: at - bar_start,
                    duration: ticks,
                    value,
                    dotted,
                    tuplet,
                    notes,
                    ..Default::default()
                });
            }
            if notated_end < next {
                push_rest(beats, bar_start, notated_end, next - notated_end, subs);
            }
            cursor = next;
        }
        if cursor < bar_end && !beats.is_empty() {
            push_rest(beats, bar_start, cursor, bar_end - cursor, subs);
        }
    }
    ScoreTrack {
        name: spec.name.clone(),
        tuning: spec.tuning.clone(),
        program: spec.program,
        volume: 100,
        bars: track_bars,
        ..Default::default()
    }
}

/// Bars following the beat grid's tempo (one tempo per bar).
pub fn build_bars(grid: &BeatGrid) -> Vec<MasterBar> {
    let bpb = grid.beats_per_bar.max(1);
    (0..grid.bars())
        .map(|b| {
            let secs = grid.times[(b + 1) * bpb] - grid.times[b * bpb];
            let bpm = (60.0 * bpb as f64 / secs.max(1e-3)).clamp(20.0, 400.0);
            MasterBar {
                numerator: bpb as u32,
                denominator: 4,
                tempo_changes: vec![(0, bpm)],
                ..Default::default()
            }
        })
        .collect()
}

pub fn build_score(title: &str, artist: &str, grid: &BeatGrid, tracks: Vec<ScoreTrack>) -> Score {
    let bars = build_bars(grid);
    Score {
        title: title.to_string(),
        artist: artist.to_string(),
        initial_tempo: bars
            .first()
            .and_then(|b| b.tempo_changes.first())
            .map(|t| t.1)
            .unwrap_or(120.0),
        bars,
        tracks,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: f64, end: f64) -> NoteEstimate {
        NoteEstimate {
            start,
            end,
            pitch,
            amplitude: 0.8,
        }
    }

    /// 120 BPM grid in 4/4: a beat every 0.5 s, `bars` bars.
    fn grid(bars: usize) -> BeatGrid {
        BeatGrid {
            times: (0..=bars * 4).map(|i| i as f64 * 0.5).collect(),
            beats_per_bar: 4,
        }
    }

    fn event(tick: i64, end_tick: i64, pitches: &[(u8, f32)]) -> GridEvent {
        GridEvent {
            tick,
            end_tick,
            secs: tick as f64 / 1920.0,
            pitches: pitches.to_vec(),
        }
    }

    #[test]
    fn quantizes_to_sixteenths_and_groups_chords() {
        let notes = vec![
            note(40, 0.01, 0.48),
            note(47, -0.01, 0.48),
            note(52, 0.02, 0.48),  // a strum: one chord
            note(45, 0.135, 0.25), // ~ the 2nd 16th (0.125 s)
            note(45, 0.98, 1.4),   // beat 3, slightly early
        ];
        let q = quantize(&notes, &grid(1), 6, &QuantizeParams::default());
        assert_eq!(q.subdivisions, vec![STRAIGHT; 4]);
        let ticks: Vec<i64> = q.events.iter().map(|e| e.tick).collect();
        assert_eq!(ticks, vec![0, 240, 1920]);
        assert_eq!(q.events[0].pitches.len(), 3);
        assert_eq!(q.events[0].end_tick, 960);
    }

    #[test]
    fn triplets_are_detected_per_beat() {
        // Beat 1: three even notes (triplet 8ths); beat 2: 8ths; beat 3: a
        // quarter; beat 4: four 16ths.
        let mut notes = Vec::new();
        for k in 0..3 {
            notes.push(note(50 + k, k as f64 * 0.5 / 3.0, 0.0));
        }
        notes.push(note(55, 0.5, 0.0));
        notes.push(note(57, 0.75, 0.0));
        notes.push(note(59, 1.0, 0.0));
        for k in 0..4 {
            notes.push(note(60 + k, 1.5 + k as f64 * 0.125, 0.0));
        }
        for n in &mut notes {
            n.end = n.start + 0.1;
        }
        let q = quantize(&notes, &grid(1), 6, &QuantizeParams::default());
        assert_eq!(q.subdivisions, vec![TRIPLET, STRAIGHT, STRAIGHT, STRAIGHT]);
        let ticks: Vec<i64> = q.events.iter().map(|e| e.tick).collect();
        assert_eq!(
            ticks,
            vec![0, 320, 640, 960, 1440, 1920, 2880, 3120, 3360, 3600]
        );
    }

    #[test]
    fn one_off_beat_note_stays_straight() {
        // A single note late in the beat isn't enough evidence for triplets.
        let notes = vec![note(50, 0.0, 0.1), note(52, 0.18, 0.3)];
        let q = quantize(&notes, &grid(1), 6, &QuantizeParams::default());
        assert_eq!(q.subdivisions[0], STRAIGHT);
        assert_eq!(q.events[1].tick, 240);
    }

    #[test]
    fn notation_uses_tuplets_only_inside_triplet_beats() {
        let subs = [TRIPLET, STRAIGHT, TRIPLET, STRAIGHT];
        // A triplet 8th, then 8th + quarter triplet.
        assert_eq!(notate(0, 320, &subs), vec![(0, 320, 8, false, Some(3))]);
        assert_eq!(notate(320, 640, &subs), vec![(320, 640, 4, false, Some(3))]);
        // From the last triplet 8th of beat 1 through beat 2: triplet + quarter.
        assert_eq!(
            notate(640, 1280, &subs),
            vec![(640, 320, 8, false, Some(3)), (960, 960, 4, false, None)]
        );
        // A whole triplet beat is a plain quarter, merged with straight time.
        assert_eq!(notate(960, 2880, &subs), vec![(960, 2880, 2, true, None)]);
    }

    #[test]
    fn chords_get_playable_fingerings() {
        let spec = guitar_spec(&[]);
        let events = vec![
            event(0, 960, &[(40, 0.9), (47, 0.8), (52, 0.7)]),
            // An open A major shape: A2 E3 A3 C#4 E4.
            event(
                960,
                1920,
                &[(45, 0.9), (52, 0.9), (57, 0.9), (61, 0.9), (64, 0.9)],
            ),
            // Out of range for guitar: dropped.
            event(1920, 2880, &[(20, 0.9)]),
        ];
        let frets = fret_events(&events, &spec);
        let power: Vec<(u8, i16)> = frets[0].iter().map(|n| (n.1, n.2)).collect();
        assert_eq!(power, vec![(6, 0), (5, 2), (4, 2)]);
        let a_major: Vec<(u8, i16)> = frets[1].iter().map(|n| (n.1, n.2)).collect();
        assert_eq!(a_major, vec![(5, 0), (4, 2), (3, 2), (2, 2), (1, 0)]);
        assert!(frets[2].is_empty());
    }

    #[test]
    fn melodies_stay_in_one_hand_position() {
        // A minor pentatonic run around the 5th fret: A3 C4 D4 E4 G4 A4.
        let spec = guitar_spec(&[]);
        let events: Vec<GridEvent> = [57u8, 60, 62, 64, 67, 69]
            .iter()
            .enumerate()
            .map(|(i, p)| event(i as i64 * 240, (i as i64 + 1) * 240, &[(*p, 0.8)]))
            .collect();
        let frets = fret_events(&events, &spec);
        let fretted: Vec<i16> = frets.iter().map(|f| f[0].2).filter(|f| *f > 0).collect();
        assert!(
            fretted.windows(2).all(|w| (w[0] - w[1]).abs() <= 4),
            "frets {fretted:?}"
        );
    }

    #[test]
    fn low_notes_pick_a_lower_tuning() {
        let drop_d: Vec<NoteEstimate> = (0..60)
            .map(|i| note(if i % 3 == 0 { 38 } else { 50 }, i as f64, i as f64 + 0.5))
            .collect();
        assert_eq!(guitar_spec(&drop_d).tuning, vec![64, 59, 55, 50, 45, 38]);
        assert_eq!(bass_spec(&[note(23, 0.0, 1.0)]).tuning.len(), 5);
        assert_eq!(guitar_spec(&[note(31, 0.0, 1.0)]).tuning.len(), 8);
    }

    #[test]
    fn splits_lengths_into_notated_values() {
        assert_eq!(split_steps(16), vec![(1, false, 16)]);
        assert_eq!(split_steps(6), vec![(4, true, 6)]);
        assert_eq!(split_steps(5), vec![(4, false, 4), (16, false, 1)]);
        assert_eq!(split_steps(15).iter().map(|s| s.2).sum::<usize>(), 15);
    }

    fn assert_tiled(track: &ScoreTrack, bar_ticks: i64) {
        for bar in &track.bars {
            let total: i64 = bar.voices[0].iter().map(|b| b.duration).sum();
            assert!(total == 0 || total == bar_ticks, "bar ticks {total}");
            let mut expected_start = 0;
            for beat in &bar.voices[0] {
                assert_eq!(beat.start, expected_start);
                expected_start += beat.duration;
            }
        }
    }

    #[test]
    fn bars_are_fully_tiled_and_ties_continue_notes() {
        let spec = guitar_spec(&[]);
        let notes = vec![note(45, 0.0, 0.6), note(52, 0.625, 2.0), note(57, 2.5, 2.6)];
        let g = grid(2);
        let q = quantize(&notes, &g, 6, &QuantizeParams::default());
        let frets = fret_events(&q.events, &spec);
        let track = build_track(&spec, &q, &frets, &g);
        assert_tiled(&track, 3840);
        // The note at step 5 lasts 11 steps in bar 1: split into notated
        // pieces with ties after the first.
        let bar0 = &track.bars[0].voices[0];
        let second = bar0.iter().position(|b| b.start == 5 * 240).unwrap();
        assert!(!bar0[second].notes[0].tie);
        assert!(bar0[second + 1].notes.iter().all(|n| n.tie));
        // Bar 2: the short note at beat 2 is followed by a rest.
        let bar1 = &track.bars[1].voices[0];
        let with_note = bar1.iter().find(|b| !b.notes.is_empty()).unwrap();
        assert_eq!(with_note.start, 4 * 240);
        assert!(bar1.last().unwrap().notes.is_empty());

        let score = build_score("T", "A", &g, vec![track]);
        assert_eq!(score.bars.len(), 2);
        assert_eq!(score.bars[0].numerator, 4);
        assert!((score.initial_tempo - 120.0).abs() < 1e-9);
    }

    #[test]
    fn waltz_bars_have_three_beats_and_triplets_tile() {
        let spec = guitar_spec(&[]);
        let g = BeatGrid {
            times: (0..=6).map(|i| i as f64 * 0.5).collect(),
            beats_per_bar: 3,
        };
        // Bar 1: triplet 8ths on beat 1, then a half note. Bar 2: quarters.
        let mut notes: Vec<NoteEstimate> = (0..3)
            .map(|k| note(52 + k, k as f64 / 6.0, k as f64 / 6.0 + 0.15))
            .collect();
        notes.push(note(57, 0.5, 1.5));
        for k in 0..3 {
            notes.push(note(60, 1.5 + k as f64 * 0.5, 1.9 + k as f64 * 0.5));
        }
        let q = quantize(&notes, &g, 6, &QuantizeParams::default());
        assert_eq!(q.subdivisions[0], TRIPLET);
        let frets = fret_events(&q.events, &spec);
        let track = build_track(&spec, &q, &frets, &g);
        assert_tiled(&track, 2880);
        let bar0 = &track.bars[0].voices[0];
        assert!(
            bar0[..3]
                .iter()
                .all(|b| b.tuplet == Some(3) && b.value == 8)
        );
        assert_eq!(
            (bar0[3].start, bar0[3].value, bar0[3].tuplet),
            (960, 2, None)
        );
        let score = build_score("T", "A", &g, vec![track]);
        assert_eq!(score.bars[0].numerator, 3);
        // Triplets and 3/4 survive the trip through Guitar Pro 5.
        let bytes = crate::shared::tab::export::score_to_gp5(&score).unwrap();
        let back = crate::shared::tab::import::load_score_bytes(&bytes, Some("gp5")).unwrap();
        assert_eq!(back.bars[0].numerator, 3);
        let shape = |s: &Score| -> Vec<(i64, i64, Option<u8>, usize)> {
            s.tracks[0].bars[0].voices[0]
                .iter()
                .map(|b| (b.start, b.duration, b.tuplet, b.notes.len()))
                .collect()
        };
        assert_eq!(shape(&back), shape(&score));
    }
}
