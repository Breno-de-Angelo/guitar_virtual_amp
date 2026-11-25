//! Playing techniques read from the pitch contour of each note: bends (and
//! releases), vibrato, slides and hammer-ons/pull-offs, in the spirit of
//! the F0-pattern rules of Chen, Su and Yang ("Electric guitar playing
//! technique detection in real-world recordings based on F0 sequence
//! pattern recognition", ISMIR 2015) and of TENT (Su et al., TISMIR 2019),
//! which feeds the detected technique back into note tracking.
//!
//! The contour is Basic Pitch's third output: pitch salience at 1/3 of a
//! semitone, ~86 frames per second, which follows a bend or a slide that
//! the note posteriors round to the nearest semitone. The note models often
//! split a bend into two notes (the fretted pitch, then the bent one with
//! no new attack); those are merged back into one bent note.
//!
//! Rules, on single-note passages only (chord tones confuse the contour):
//! - bend: the pitch glides up 0.6 to 3.4 semitones from where the note
//!   starts (near its semitone), the middle of the rise taking at least
//!   20 ms, without settling on the semitones in between when it rises
//!   more than 1.5; a release is the glide back down;
//! - vibrato: at least two regular cycles of a 3.5-9 Hz oscillation of at
//!   least 0.12 semitone around the pitch's trend, somewhere in the note;
//! - slide: a glide to the next note that steps through the frets in
//!   between, or any downward glide, or one longer than 3 semitones;
//! - hammer-on/pull-off: the next note follows on the spot, the pitch jumps
//!   between them, and the new note has no pick attack (no burst of
//!   high-frequency energy).
//!
//! Palm muting isn't detected yet.

use std::ops::Range;

use super::basic_pitch::{BINS_PER_SEMITONE, CONTOUR_BINS, NoteEstimate, Posteriors};

/// Techniques of one note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Technique {
    /// Bend height in quarter tones (2 = half step, 4 = whole step); 0 = no
    /// bend.
    pub bend: u8,
    /// The bend comes back down before the note ends.
    pub release: bool,
    pub vibrato: bool,
    /// Slides into the next note (on the same string).
    pub slide_out: bool,
    /// Hammer-on or pull-off into the next note (same string, not picked).
    pub legato_out: bool,
}

impl Technique {
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Links this note to the next one on the same string.
    pub fn links_next(&self) -> bool {
        self.slide_out || self.legato_out
    }
}

const LOWEST_MIDI: f32 = 21.0;
/// MIDI pitch `p` peaks at contour bin `3 * (p - 21) + 1`: the contour grid
/// starts a third of a semitone below A0 (measured with pure tones from E2
/// to C6, see `contour_bins_line_up_with_midi_pitches`).
const OFFSET_BINS: f32 = 1.0;
/// Contour salience below this is "no pitch here".
const VOICED: f32 = 0.2;
/// Notes starting closer than this are struck together.
const TOGETHER: f64 = 0.04;
/// Shortest glide that is a bend or a slide (a hammer-on jumps in 1-2
/// frames).
const MIN_GLIDE: f64 = 0.03;
/// Mean distance of the gliding pitch to the nearest semitone above which
/// it is a smooth bend; slides spend their time on the frets.
const STAIRCASE: f32 = 0.15;
/// High-frequency onset energy ratio below which a note wasn't picked.
const PICK_ATTACK: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TechniqueParams {
    pub bends: bool,
    pub vibrato: bool,
    pub slides: bool,
    pub legato: bool,
}

impl Default for TechniqueParams {
    fn default() -> Self {
        Self {
            bends: true,
            vibrato: true,
            slides: true,
            legato: true,
        }
    }
}

/// Pitch salience over time ([`Posteriors::contours`]) plus the audio, for
/// pick attacks.
pub struct Evidence<'a> {
    pub post: &'a Posteriors,
    /// Mono audio of the stem.
    pub audio: &'a [f32],
    pub rate: u32,
}

fn frames(post: &Posteriors, from: f64, to: f64) -> Range<usize> {
    let a = post.times.partition_point(|t| *t < from);
    let b = post.times.partition_point(|t| *t <= to);
    a..b.max(a)
}

fn bin_of(pitch: f32) -> f32 {
    (pitch - LOWEST_MIDI) * BINS_PER_SEMITONE as f32 + OFFSET_BINS
}

fn pitch_of(bin: f32) -> f32 {
    LOWEST_MIDI + (bin - OFFSET_BINS) / BINS_PER_SEMITONE as f32
}

/// Fractional MIDI pitch of the salience peak near `pitch` (between
/// `pitch + lo` and `pitch + hi` semitones) in every frame of `range`,
/// skipping the bins of `masked` pitches. Prefers the peak closest to the
/// previous frame's, so a bend is followed rather than a louder neighbour.
fn follow(
    post: &Posteriors,
    range: Range<usize>,
    pitch: u8,
    lo: f32,
    hi: f32,
    masked: &[u8],
) -> Vec<Option<f32>> {
    let center = bin_of(pitch as f32);
    let lo_bin = (center + lo * BINS_PER_SEMITONE as f32).round().max(1.0) as usize;
    let hi_bin = ((center + hi * BINS_PER_SEMITONE as f32).round() as usize).min(CONTOUR_BINS - 2);
    let masked_bins: Vec<f32> = masked.iter().map(|p| bin_of(*p as f32)).collect();
    let usable = |b: usize| masked_bins.iter().all(|m| (b as f32 - m).abs() > 1.5);
    let mut out = Vec::with_capacity(range.len());
    let mut previous: Option<f32> = None;
    for i in range {
        let Some(s) = post.contours.get(i) else {
            out.push(None);
            continue;
        };
        if lo_bin > hi_bin {
            out.push(None);
            continue;
        }
        let peaks: Vec<usize> = (lo_bin..=hi_bin)
            .filter(|&b| usable(b) && s[b] >= s[b - 1] && s[b] >= s[b + 1] && s[b] >= VOICED)
            .collect();
        let Some(&loudest) = peaks.iter().max_by(|a, b| s[**a].total_cmp(&s[**b])) else {
            out.push(None);
            previous = None;
            continue;
        };
        let chosen = match previous {
            Some(prev) => peaks
                .iter()
                .copied()
                .filter(|&b| (b as f32 - prev).abs() <= 2.0 && s[b] >= 0.6 * s[loudest])
                .min_by(|a, b| {
                    (*a as f32 - prev)
                        .abs()
                        .total_cmp(&(*b as f32 - prev).abs())
                })
                .unwrap_or(loudest),
            None => loudest,
        };
        let (l, c, r) = (s[chosen - 1], s[chosen], s[chosen + 1]);
        let denom = l - 2.0 * c + r;
        let shift = if denom.abs() > 1e-6 {
            (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let bin = chosen as f32 + shift;
        previous = Some(bin);
        out.push(Some(pitch_of(bin)));
    }
    out
}

/// Median of 5 over the voiced frames (unvoiced stay unvoiced).
fn smooth(track: &[Option<f32>]) -> Vec<Option<f32>> {
    (0..track.len())
        .map(|i| {
            track[i]?;
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(track.len());
            let mut v: Vec<f32> = track[lo..hi].iter().flatten().copied().collect();
            v.sort_by(f32::total_cmp);
            Some(v[v.len() / 2])
        })
        .collect()
}

fn voiced_share(track: &[Option<f32>]) -> f32 {
    track.iter().filter(|v| v.is_some()).count() as f32 / track.len().max(1) as f32
}

/// A glide of the (smoothed) offsets from about 0 to about `target`
/// semitones: `(first frame of the rise, last frame of the rise)`.
fn glide(offsets: &[Option<f32>], target: f32) -> Option<(usize, usize)> {
    let up = target > 0.0;
    let reached = |x: f32| {
        if up {
            x >= target - 0.35
        } else {
            x <= target + 0.35
        }
    };
    let at_start = |x: f32| x.abs() <= 0.35;
    let end = offsets.iter().position(|v| v.is_some_and(reached))?;
    let start = offsets[..end]
        .iter()
        .rposition(|v| v.is_some_and(at_start))?;
    // The rise has to be voiced most of the way.
    let gap = offsets[start..=end].iter().filter(|v| v.is_none()).count();
    (gap * 3 <= end - start + 1).then_some((start, end))
}

/// Mean distance to the nearest whole semitone of the pitch while gliding
/// (`None` when the glide is too short to tell).
fn staircase(offsets: &[Option<f32>], rise: (usize, usize)) -> Option<f32> {
    let inner: Vec<f32> = offsets[rise.0 + 1..rise.1]
        .iter()
        .flatten()
        .map(|x| (x - x.round()).abs())
        .collect();
    (inner.len() >= 3).then(|| inner.iter().sum::<f32>() / inner.len() as f32)
}

fn frame_secs(post: &Posteriors) -> f64 {
    match post.times.as_slice() {
        [a, b, ..] => b - a,
        _ => 1.0 / 86.0,
    }
}

/// Vibrato window, its hop, and the half-width of the moving average the
/// pitch is detrended with (a vibrato cycle is 0.1-0.3 s).
const VIBRATO_WINDOW: f64 = 0.5;
const VIBRATO_HOP: f64 = 0.1;
const VIBRATO_TREND: f64 = 0.15;
/// Smallest peak-to-peak vibrato, in semitones (Guitar-TECHS players' is
/// about 0.2).
const VIBRATO_EXTENT: f32 = 0.12;

/// A regular 3.5-9 Hz oscillation of at least [`VIBRATO_EXTENT`] around the
/// pitch's trend, in some [`VIBRATO_WINDOW`] of the note (guitarists often
/// start the vibrato late in a held note).
fn has_vibrato(offsets: &[Option<f32>], frame: f64) -> bool {
    let voiced: Vec<f32> = offsets.iter().flatten().copied().collect();
    let window = (VIBRATO_WINDOW / frame).round() as usize;
    if voiced.len() < window || voiced_share(offsets) < 0.8 {
        return false;
    }
    let half = ((VIBRATO_TREND / frame).round() as usize).max(2);
    let detrended: Vec<f32> = (0..voiced.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(voiced.len());
            voiced[i] - voiced[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect();
    let hop = ((VIBRATO_HOP / frame).round() as usize).max(1);
    (0..=detrended.len() - window)
        .step_by(hop)
        .any(|start| oscillates(&detrended[start..start + window], frame))
}

/// At least two regular cycles at 3.5-9 Hz of at least [`VIBRATO_EXTENT`].
fn oscillates(x: &[f32], frame: f64) -> bool {
    let mut sorted = x.to_vec();
    sorted.sort_by(f32::total_cmp);
    let extent = sorted[sorted.len() * 95 / 100] - sorted[sorted.len() * 5 / 100];
    if extent < VIBRATO_EXTENT {
        return false;
    }
    // Zero crossings, ignoring wiggles inside a dead band.
    let band = 0.2 * extent;
    let mut crossings = Vec::new();
    let mut sign = 0i8;
    for (i, &v) in x.iter().enumerate() {
        let s = if v > band {
            1
        } else if v < -band {
            -1
        } else {
            continue;
        };
        if sign != 0 && s != sign {
            crossings.push(i);
        }
        sign = s;
    }
    if crossings.len() < 4 {
        return false;
    }
    let gaps: Vec<f64> = crossings
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 * frame)
        .collect();
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let rate = 1.0 / (2.0 * mean);
    let spread =
        (gaps.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / gaps.len() as f64).sqrt() / mean;
    (3.5..=9.0).contains(&rate) && spread < 0.5
}

/// Tunable bend thresholds (`GVA_BEND_*` env vars override them, for
/// sweeps on labelled data).
#[derive(Clone, Copy, Debug)]
struct BendRule {
    /// Smallest rise above the note's starting pitch, in semitones.
    min_rise: f32,
    /// How far the starting pitch may sit from the note's semitone.
    start_tol: f32,
    /// Shortest time for the middle 60% of the rise, in seconds.
    min_glide: f64,
}

fn bend_rule() -> BendRule {
    let tune = |k: &str, d: f32| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    BendRule {
        min_rise: tune("GVA_BEND_RISE", 0.6),
        start_tol: tune("GVA_BEND_START", 0.45),
        min_glide: tune("GVA_BEND_GLIDE", 0.02) as f64,
    }
}

/// A bend within one note's pitch offsets (semitones from the note):
/// `(height in quarter tones, released)`. The pitch starts near the note,
/// rises [`BendRule::min_rise`] to 3.4 semitones above where it started
/// (the middle 60% of the rise taking at least [`BendRule::min_glide`]),
/// holds there for 60 ms, isn't a vibrato (rises under a semitone that keep
/// oscillating) and, beyond 1.5 semitones, doesn't step through the frets
/// (that is a slide).
fn bend_in_note(off: &[Option<f32>], frame: f64) -> Option<(u8, bool)> {
    let rule = bend_rule();
    let mut first: Vec<f32> = off.iter().flatten().take(4).copied().collect();
    if first.len() < 4 {
        return None;
    }
    first.sort_by(f32::total_cmp);
    let base = (first[1] + first[2]) / 2.0;
    if base.abs() > rule.start_tol {
        return None;
    }
    let (k, peak) = off
        .iter()
        .enumerate()
        .filter_map(|(k, v)| v.map(|x| (k, x)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let rise = peak - base;
    // Held up there for 60 ms (a vibrato's crests last less).
    let held = off[k..]
        .iter()
        .take_while(|v| v.is_some_and(|x| x >= base + 0.6 * rise))
        .count();
    if !(rule.min_rise..=3.4).contains(&rise) || (held as f64) * frame < 0.06 {
        return None;
    }
    // A small rise that keeps oscillating is a vibrato.
    if rise < 1.0 && has_vibrato(off, frame) {
        return None;
    }
    // The middle 60% of the rise: last frame at or below 20% before the
    // first frame at or above 80%.
    let hi = off[..=k]
        .iter()
        .position(|v| v.is_some_and(|x| x >= base + 0.8 * rise))?;
    let lo = off[..hi]
        .iter()
        .rposition(|v| v.is_some_and(|x| x <= base + 0.2 * rise))?;
    if ((hi - lo) as f64) * frame < rule.min_glide {
        return None;
    }
    if rise > 1.5 && staircase(off, (lo, hi)).is_some_and(|s| s < STAIRCASE) {
        return None;
    }
    let back = off[k..]
        .iter()
        .filter(|v| v.is_some_and(|x| x <= base + 0.3 * rise))
        .count();
    Some(((rise * 2.0).round().clamp(1.0, 7.0) as u8, back >= 3))
}

/// High-frequency energy (2nd-order high-pass at ~2 kHz) in 5 ms hops.
struct Attacks {
    energy: Vec<f32>,
    hop_secs: f64,
}

impl Attacks {
    fn new(audio: &[f32], rate: u32) -> Self {
        let rate = rate.max(1);
        let hop = (rate as usize / 200).max(1);
        // RBJ high-pass biquad, Q = 0.707.
        let w0 = std::f32::consts::TAU * 2000.0_f32.min(rate as f32 * 0.45) / rate as f32;
        let alpha = w0.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = (
            (1.0 + cos) / 2.0 / a0,
            -(1.0 + cos) / a0,
            (1.0 + cos) / 2.0 / a0,
        );
        let (a1, a2) = (-2.0 * cos / a0, (1.0 - alpha) / a0);
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut energy = Vec::with_capacity(audio.len() / hop + 1);
        let mut acc = 0.0f32;
        for (i, &x) in audio.iter().enumerate() {
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x, y1, y);
            acc += y * y;
            if (i + 1) % hop == 0 {
                energy.push(acc / hop as f32);
                acc = 0.0;
            }
        }
        Self {
            energy,
            hop_secs: hop as f64 / rate as f64,
        }
    }

    fn mean(&self, from: f64, to: f64) -> f32 {
        let a = (from / self.hop_secs).floor().max(0.0) as usize;
        let b = ((to / self.hop_secs).ceil() as usize).min(self.energy.len());
        if a >= b {
            return 0.0;
        }
        self.energy[a..b].iter().sum::<f32>() / (b - a) as f32
    }

    /// High-frequency energy just after `t` over just before it.
    fn ratio(&self, t: f64) -> f32 {
        let after = self.mean(t - 0.005, t + 0.025);
        let before = self.mean(t - 0.045, t - 0.012);
        after / (before + 1e-9)
    }
}

/// Indices of the notes starting with note `i` (itself excluded).
fn struck_with(notes: &[NoteEstimate], alive: &[bool], i: usize) -> bool {
    notes
        .iter()
        .enumerate()
        .any(|(j, n)| j != i && alive[j] && (n.start - notes[i].start).abs() < TOGETHER)
}

/// Pitches of other notes sounding when note `i` starts (chord tones and
/// ringing notes), whose bins the contour follower must ignore.
fn ringing_with(notes: &[NoteEstimate], alive: &[bool], i: usize) -> Vec<u8> {
    let n = &notes[i];
    notes
        .iter()
        .enumerate()
        .filter(|(j, o)| {
            *j != i
                && alive[*j]
                && o.start < n.start + TOGETHER
                && o.end > n.start + 0.02
                && o.pitch != n.pitch
        })
        .map(|(_, o)| o.pitch)
        .collect()
}

fn offsets(track: &[Option<f32>], pitch: u8) -> Vec<Option<f32>> {
    track.iter().map(|v| v.map(|p| p - pitch as f32)).collect()
}

/// Detects techniques and merges notes that are one bent note. Returns the
/// notes (sorted by start) and their techniques, index for index.
pub fn detect(
    notes: Vec<NoteEstimate>,
    evidence: &Evidence,
    params: &TechniqueParams,
) -> (Vec<NoteEstimate>, Vec<Technique>) {
    let post = evidence.post;
    let mut notes = notes;
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    let n = notes.len();
    let mut tech = vec![Technique::default(); n];
    if post.contours.len() < 8 || n == 0 {
        return (notes, tech);
    }
    let frame = frame_secs(post);
    let mut alive = vec![true; n];

    // 1. Bends the note model split in two (or three, with the release).
    if params.bends {
        for i in 0..n {
            if !alive[i] || struck_with(&notes, &alive, i) {
                continue;
            }
            let masked = ringing_with(&notes, &alive, i);
            let Some(j) = (i + 1..n).find(|&j| {
                let (a, b) = (&notes[i], &notes[j]);
                alive[j]
                    && b.start > a.start + TOGETHER
                    && b.start < a.end + 0.05
                    && (1..=3).contains(&(b.pitch as i16 - a.pitch as i16))
            }) else {
                continue;
            };
            if struck_with(&notes, &alive, j) {
                continue;
            }
            let (a, b) = (notes[i], notes[j]);
            let delta = (b.pitch - a.pitch) as f32;
            let range = frames(post, a.start + 0.015, b.start + 0.06);
            let track = smooth(&follow(post, range, a.pitch, -0.7, delta + 0.7, &masked));
            let off = offsets(&track, a.pitch);
            let Some(rise) = glide(&off, delta) else {
                continue;
            };
            let secs = (rise.1 - rise.0) as f64 * frame;
            let smooth_rise = staircase(&off, rise).is_none_or(|s| s >= STAIRCASE);
            if secs < MIN_GLIDE || !smooth_rise {
                continue;
            }
            alive[j] = false;
            notes[i].end = notes[i].end.max(b.end);
            notes[i].amplitude = notes[i].amplitude.max(b.amplitude);
            tech[i].bend = (delta * 2.0) as u8;
            // A release split off as a third note at the fretted pitch.
            if let Some(k) = (j + 1..n).find(|&k| {
                let c = &notes[k];
                alive[k]
                    && c.pitch == a.pitch
                    && c.start > b.start + TOGETHER
                    && c.start < b.end + 0.05
            }) {
                let c = notes[k];
                let range = frames(post, b.start, c.start + 0.06);
                let back = smooth(&follow(post, range, b.pitch, -delta - 0.7, 0.7, &masked));
                if glide(&offsets(&back, b.pitch), -delta).is_some()
                    && !struck_with(&notes, &alive, k)
                {
                    alive[k] = false;
                    notes[i].end = notes[i].end.max(c.end);
                    tech[i].release = true;
                }
            }
        }
    }

    // 2. Bends and vibrato within one note.
    for i in 0..n {
        if !alive[i] || struck_with(&notes, &alive, i) {
            continue;
        }
        let a = notes[i];
        let masked = ringing_with(&notes, &alive, i);
        let range = frames(post, a.start + 0.015, a.end);
        if range.len() < 6 {
            continue;
        }
        let track = smooth(&follow(post, range, a.pitch, -0.7, 3.7, &masked));
        let off = offsets(&track, a.pitch);
        if params.bends
            && tech[i].bend == 0
            && let Some((height, release)) = bend_in_note(&off, frame)
        {
            tech[i].bend = height;
            tech[i].release = release;
        }
        if params.vibrato {
            let settle = ((0.06 / frame) as usize).min(off.len());
            tech[i].vibrato = has_vibrato(&off[settle..], frame);
        }
    }

    // 3. Slides and hammer-ons/pull-offs between consecutive single notes.
    let attacks = (params.legato).then(|| Attacks::new(evidence.audio, evidence.rate));
    let order: Vec<usize> = (0..n).filter(|&i| alive[i]).collect();
    for w in order.windows(2) {
        let (i, j) = (w[0], w[1]);
        let (a, b) = (notes[i], notes[j]);
        if struck_with(&notes, &alive, i)
            || struck_with(&notes, &alive, j)
            || a.pitch == b.pitch
            || tech[i].bend > 0
        {
            continue;
        }
        // The model only starts the target of a slide once the pitch
        // settles, so slides may leave a gap that a hammer-on can't.
        let gap = b.start - a.end;
        if !(-0.08..=0.12).contains(&gap) || b.start - a.start < 0.05 {
            continue;
        }
        // Nothing else may ring across the change.
        let others = notes.iter().enumerate().any(|(k, o)| {
            k != i && k != j && alive[k] && o.start < b.start && o.end > b.start - 0.02
        });
        if others {
            continue;
        }
        let delta = b.pitch as f32 - a.pitch as f32;
        let from = (a.start + 0.015).max(b.start - 0.3);
        let range = frames(post, from, b.start + 0.06);
        let (lo, hi) = (delta.min(0.0) - 0.7, delta.max(0.0) + 0.7);
        let track = smooth(&follow(post, range, a.pitch, lo, hi, &[]));
        let off = offsets(&track, a.pitch);
        let rise = glide(&off, delta);
        let glide_secs = rise.map(|r| (r.1 - r.0) as f64 * frame).unwrap_or(0.0);
        if params.slides && delta.abs() <= 12.0 && glide_secs >= MIN_GLIDE {
            // Up to 3 semitones smoothly is a bend that wasn't split;
            // stepping through the frets (or longer, or downward) is a slide.
            let stepped = rise
                .and_then(|r| staircase(&off, r))
                .is_some_and(|s| s < STAIRCASE);
            if delta < 0.0 || delta > 3.0 || stepped {
                tech[i].slide_out = true;
                continue;
            }
        }
        if params.legato && delta.abs() <= 5.0 && glide_secs < MIN_GLIDE && gap <= 0.04 {
            let attacks = attacks.as_ref().expect("legato on");
            if attacks.ratio(b.start) < PICK_ATTACK {
                tech[i].legato_out = true;
            }
        }
    }

    let kept: Vec<(NoteEstimate, Technique)> = notes
        .into_iter()
        .zip(tech)
        .zip(alive)
        .filter_map(|(nt, keep)| keep.then_some(nt))
        .collect();
    kept.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::basic_pitch::{self, SAMPLE_RATE};
    use std::sync::atomic::AtomicBool;

    const SR: f64 = SAMPLE_RATE as f64;

    fn hz(midi: f64) -> f64 {
        440.0 * 2f64.powf((midi - 69.0) / 12.0)
    }

    /// A plucked-string-like tone whose pitch follows `pitch(t)` (MIDI),
    /// from `start` for `len` seconds, with an optional pick noise burst.
    fn tone(out: &mut [f32], start: f64, len: f64, pick: bool, pitch: &dyn Fn(f64) -> f64) {
        let first = (start * SR) as usize;
        let n = ((len * SR) as usize).min(out.len().saturating_sub(first));
        let mut phase = 0.0f64;
        let mut seed = 12345u32;
        for i in 0..n {
            let t = i as f64 / SR;
            phase += std::f64::consts::TAU * hz(pitch(t)) / SR;
            let env = (-t * 2.0).exp() * (1.0 - (-t * 400.0).exp());
            let mut v = 0.0;
            for h in 1..=16 {
                v += (phase * h as f64).sin() / h as f64;
            }
            let mut s = 0.25 * env * v;
            if pick && t < 0.012 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                s += ((seed >> 16) as f64 / 32768.0 - 1.0) * 0.3 * (1.0 - t / 0.012);
            }
            out[first + i] += s as f32;
        }
    }

    fn analyse(audio: &[f32], notes: Vec<NoteEstimate>) -> (Vec<NoteEstimate>, Vec<Technique>) {
        let post = basic_pitch::posteriors(audio, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let evidence = Evidence {
            post: &post,
            audio,
            rate: SAMPLE_RATE,
        };
        detect(notes, &evidence, &TechniqueParams::default())
    }

    fn note(pitch: u8, start: f64, end: f64) -> NoteEstimate {
        NoteEstimate {
            start,
            end,
            pitch,
            amplitude: 0.7,
        }
    }

    #[test]
    fn contour_bins_line_up_with_midi_pitches() {
        for pitch in [40u8, 57, 76] {
            let mut audio = vec![0f32; (2.0 * SR) as usize];
            tone(&mut audio, 0.2, 1.5, true, &|_| pitch as f64);
            let post =
                basic_pitch::posteriors(&audio, &mut |_| {}, &AtomicBool::new(false)).unwrap();
            let range = frames(&post, 0.4, 1.0);
            let track = follow(&post, range, pitch, -2.0, 2.0, &[]);
            let voiced: Vec<f32> = track.iter().flatten().copied().collect();
            assert!(voiced.len() > 20, "{track:?}");
            let mean = voiced.iter().sum::<f32>() / voiced.len() as f32;
            assert!(
                (mean - pitch as f32).abs() < 0.1,
                "{pitch}: mean pitch {mean}"
            );
        }
    }

    #[test]
    fn a_split_bend_becomes_one_bent_note() {
        // E4 picked, bent up a whole step from 0.3 s to 0.45 s, held.
        let mut audio = vec![0f32; (2.0 * SR) as usize];
        let bend = |t: f64| 64.0 + 2.0 * ((t - 0.3) / 0.15).clamp(0.0, 1.0);
        tone(&mut audio, 0.2, 1.4, true, &|t| bend(t + 0.2));
        let (notes, tech) = analyse(&audio, vec![note(64, 0.2, 0.55), note(66, 0.5, 1.5)]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(tech[0].bend, 4, "{tech:?}");
        assert!(!tech[0].release);
        assert!(notes[0].end >= 1.49);
    }

    #[test]
    fn a_bend_inside_one_note_and_its_release() {
        let mut audio = vec![0f32; (2.5 * SR) as usize];
        // Half-step bend at 0.5 s, released at 1.2 s.
        let shape = |t: f64| {
            let up = ((t - 0.3) / 0.12).clamp(0.0, 1.0);
            let down = ((t - 1.0) / 0.12).clamp(0.0, 1.0);
            62.0 + up - down
        };
        tone(&mut audio, 0.2, 1.8, true, &shape);
        let (notes, tech) = analyse(&audio, vec![note(62, 0.2, 1.9)]);
        assert_eq!(notes.len(), 1);
        assert_eq!(tech[0].bend, 2, "{tech:?}");
        assert!(tech[0].release, "{tech:?}");
    }

    #[test]
    fn vibrato_is_detected_and_steady_notes_are_left_alone() {
        let mut audio = vec![0f32; (3.0 * SR) as usize];
        tone(&mut audio, 0.2, 1.2, true, &|t| {
            60.0 + 0.3 * (std::f64::consts::TAU * 6.0 * t).sin()
        });
        tone(&mut audio, 1.6, 1.2, true, &|_| 67.0);
        let (_, tech) = analyse(&audio, vec![note(60, 0.2, 1.4), note(67, 1.6, 2.8)]);
        assert!(tech[0].vibrato, "{tech:?}");
        assert!(tech[1].is_none(), "{tech:?}");
    }

    #[test]
    fn slides_step_through_the_frets() {
        // A3 picked, slid up 4 frets (25 ms per fret) to C#4 with no new attack.
        let mut audio = vec![0f32; (2.0 * SR) as usize];
        let slide = |t: f64| 57.0 + ((t - 0.4) / 0.025).floor().clamp(0.0, 4.0);
        tone(&mut audio, 0.2, 1.4, true, &|t| slide(t + 0.2));
        let (notes, tech) = analyse(&audio, vec![note(57, 0.2, 0.62), note(61, 0.62, 1.6)]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(tech[0].slide_out, "{tech:?}");
        assert_eq!(tech[0].bend, 0);
        // The model often leaves the glide itself out of both notes.
        let (_, tech) = analyse(&audio, vec![note(57, 0.2, 0.42), note(61, 0.53, 1.6)]);
        assert!(tech[0].slide_out && !tech[0].legato_out, "{tech:?}");
    }

    #[test]
    fn hammer_ons_have_no_pick_attack() {
        let mut audio = vec![0f32; (3.0 * SR) as usize];
        // Picked G3, hammered to A3 at 0.6 s: the string keeps ringing,
        // only its pitch jumps.
        tone(&mut audio, 0.2, 1.0, true, &|t| {
            if t < 0.4 { 55.0 } else { 57.0 }
        });
        // Then the same two notes, both picked.
        tone(&mut audio, 1.5, 0.4, true, &|_| 55.0);
        tone(&mut audio, 1.9, 0.6, true, &|_| 57.0);
        let notes = vec![
            note(55, 0.2, 0.6),
            note(57, 0.6, 1.2),
            note(55, 1.5, 1.9),
            note(57, 1.9, 2.5),
        ];
        let (_, tech) = analyse(&audio, notes);
        assert!(tech[0].legato_out, "{tech:?}");
        assert!(!tech[2].legato_out, "{tech:?}");
        assert!(!tech[1].legato_out && !tech[3].legato_out);
        assert!(tech.iter().all(|t| !t.slide_out && t.bend == 0), "{tech:?}");
    }

    #[test]
    fn chords_are_left_alone() {
        let mut audio = vec![0f32; (2.0 * SR) as usize];
        for p in [52.0, 59.0, 64.0] {
            tone(&mut audio, 0.2, 1.5, true, &move |_| p);
        }
        let notes = vec![note(52, 0.2, 1.7), note(59, 0.2, 1.7), note(64, 0.21, 1.7)];
        let (out, tech) = analyse(&audio, notes);
        assert_eq!(out.len(), 3);
        assert!(tech.iter().all(Technique::is_none), "{tech:?}");
    }
}
