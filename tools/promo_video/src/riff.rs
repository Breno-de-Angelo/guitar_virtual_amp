//! Riffs and drum patterns in text notation, turned into played notes (for
//! the guitar sampler and the synth) and into Guitar Pro tracks (for the tab
//! player scenes), so what is heard and what is shown match.
//!
//! Riff notation: tokens separated by spaces, one per step (an eighth note
//! with `step = 8`, a sixteenth with `step = 16`), in 4/4.
//! - `6:0` string 6 (low E), fret 0; `6:0+5:2+4:2` a chord (strummed)
//! - suffixes: `m` palm mute, `~` vibrato, `!` accent (`6:0+5:2m`, `1:5~`)
//! - `.` a rest, `-` holds the previous note or rest one more step
//! - `|` is ignored (write bar lines for readability)
//!
//! Drum notation: one line per drum, one character-token per step:
//! `x` hit, `X` accent, `o` soft (ghost), `.` nothing.

use guitar_core::shared::tab::model::{MasterBar, Score, ScoreTrack, TabBeat, TabNote, TrackBar, TICKS_PER_QUARTER};

/// Open strings, string 1 (highest) first.
pub const GUITAR: [u8; 6] = [64, 59, 55, 50, 45, 40];
pub const BASS: [u8; 4] = [43, 38, 33, 28];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub string: u8,
    pub fret: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// First step and length in steps.
    pub step: usize,
    pub len: usize,
    /// Empty = rest.
    pub notes: Vec<Hit>,
    pub mute: bool,
    pub vibrato: bool,
    pub accent: bool,
}

/// One note as played, seconds from the riff's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Played {
    pub start: f64,
    pub end: f64,
    pub string: u8,
    pub fret: u8,
    pub pitch: u8,
    /// 0..1.
    pub velocity: f32,
    pub mute: bool,
    pub vibrato: bool,
    /// Strummed down (on-beat steps) or up (the steps between).
    pub down: bool,
}

#[derive(Clone, Debug)]
pub struct Riff {
    pub bpm: f64,
    /// Steps per whole note (8 = eighth notes).
    pub step: u32,
    pub events: Vec<Event>,
    pub steps: usize,
    pub tuning: Vec<u8>,
    /// Notes ring until the next note on the same string (arpeggios).
    pub ring: bool,
}

/// `s:f` -> hit, for an instrument with `strings` strings.
pub fn parse_position(token: &str, strings: usize) -> Result<Hit, String> {
    let (s, f) = token.split_once(':').ok_or_else(|| format!("'{token}': expected string:fret"))?;
    let string: u8 = s.parse().map_err(|_| format!("'{token}': bad string"))?;
    let fret: u8 = f.parse().map_err(|_| format!("'{token}': bad fret"))?;
    if string == 0 || string as usize > strings || fret > 24 {
        return Err(format!("'{token}': string 1-{strings}, fret 0-24"));
    }
    Ok(Hit { string, fret })
}

fn check_step(step: u32) -> Result<(), String> {
    if [4, 8, 16].contains(&step) { Ok(()) } else { Err("step must be 4, 8 or 16".into()) }
}

fn step_secs(bpm: f64, step: u32) -> f64 {
    60.0 / bpm * 4.0 / step as f64
}

impl Riff {
    pub fn parse(bpm: f64, step: u32, notes: &str, tuning: &[u8], ring: bool) -> Result<Self, String> {
        check_step(step)?;
        let mut events: Vec<Event> = Vec::new();
        let mut pos = 0;
        for token in notes.split_whitespace() {
            match token {
                "|" => continue,
                "-" => match events.last_mut() {
                    Some(e) => e.len += 1,
                    None => return Err("a riff can't start with '-'".into()),
                },
                "." => events.push(Event { step: pos, len: 1, notes: Vec::new(), mute: false, vibrato: false, accent: false }),
                _ => {
                    let body = token.trim_end_matches(['m', '~', '!']);
                    let suffix = &token[body.len()..];
                    let notes = body.split('+').map(|t| parse_position(t, tuning.len())).collect::<Result<Vec<_>, _>>()?;
                    events.push(Event {
                        step: pos,
                        len: 1,
                        notes,
                        mute: suffix.contains('m'),
                        vibrato: suffix.contains('~'),
                        accent: suffix.contains('!'),
                    });
                }
            }
            pos += 1;
        }
        if pos == 0 {
            return Err("empty riff".into());
        }
        Ok(Self { bpm, step, events, steps: pos, tuning: tuning.to_vec(), ring })
    }

    pub fn step_secs(&self) -> f64 {
        step_secs(self.bpm, self.step)
    }

    pub fn secs(&self) -> f64 {
        self.steps as f64 * self.step_secs()
    }

    pub fn pitch(&self, hit: Hit) -> u8 {
        self.tuning[hit.string as usize - 1] + hit.fret
    }

    /// The notes from 0 to `until` seconds, looped. Chords keep one entry
    /// per string; the sampler strums them.
    pub fn played(&self, until: f64) -> Vec<Played> {
        let dt = self.step_secs();
        let mut out: Vec<Played> = Vec::new();
        let mut base = 0.0;
        while base < until {
            for e in &self.events {
                let t = base + e.step as f64 * dt;
                if t >= until {
                    break;
                }
                let on_beat = (e.step * 4) % self.step as usize == 0;
                let velocity = if e.accent { 1.0 } else if on_beat { 0.88 } else { 0.74 };
                for n in &e.notes {
                    out.push(Played {
                        start: t,
                        end: t + e.len as f64 * dt,
                        string: n.string,
                        fret: n.fret,
                        pitch: self.pitch(*n),
                        velocity,
                        mute: e.mute,
                        vibrato: e.vibrato,
                        down: e.step % 2 == 0,
                    });
                }
            }
            base += self.secs();
        }
        if self.ring {
            // Let ring: each note lasts until the next one on its string.
            for i in 0..out.len() {
                let next = out[i + 1..].iter().find(|n| n.string == out[i].string).map(|n| n.start);
                out[i].end = out[i].end.max(next.unwrap_or(out[i].end + 2.0));
            }
        }
        out
    }

    /// The riff looped over `bars` bars as a Guitar Pro track.
    pub fn track(&self, name: &str, program: u8, bars: usize) -> ScoreTrack {
        let step_ticks = TICKS_PER_QUARTER * 4 / self.step as i64;
        let mut beats: Vec<(i64, i64, Vec<TabNote>)> = Vec::new();
        let total = bars as i64 * TICKS_PER_QUARTER * 4;
        let mut base = 0;
        while base < total {
            for e in &self.events {
                let notes = e
                    .notes
                    .iter()
                    .map(|n| TabNote {
                        string: n.string,
                        fret: n.fret as i16,
                        velocity: if e.accent { 115 } else { 95 },
                        palm_mute: e.mute,
                        vibrato: e.vibrato,
                        let_ring: self.ring,
                        ..TabNote::default()
                    })
                    .collect();
                beats.push((base + e.step as i64 * step_ticks, e.len as i64 * step_ticks, notes));
            }
            base += self.steps as i64 * step_ticks;
        }
        ScoreTrack {
            name: name.into(),
            tuning: self.tuning.clone(),
            program,
            volume: 100,
            bars: bar_beats(&beats, bars),
            ..ScoreTrack::default()
        }
    }
}

/// GM drum keys by lane name.
pub fn drum_key(name: &str) -> Option<u8> {
    Some(match name {
        "kick" => 36,
        "side" => 37,
        "snare" => 38,
        "hat" => 42,
        "pedal_hat" => 44,
        "open_hat" => 46,
        "tom_low" => 41,
        "tom_mid" => 45,
        "tom_hi" => 48,
        "crash" => 49,
        "ride" => 51,
        "ride_bell" => 53,
        "china" => 52,
        "splash" => 55,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct Drums {
    pub bpm: f64,
    pub step: u32,
    pub steps: usize,
    /// (GM key, velocity per step; 0 = no hit).
    pub lanes: Vec<(u8, Vec<u8>)>,
}

impl Drums {
    pub fn parse(bpm: f64, step: u32, lanes: &[(String, String)]) -> Result<Self, String> {
        check_step(step)?;
        let mut parsed = Vec::new();
        let mut steps = 0;
        for (name, line) in lanes {
            let key = drum_key(name).ok_or_else(|| format!("unknown drum '{name}'"))?;
            let hits: Vec<u8> = line
                .split_whitespace()
                .filter(|t| *t != "|")
                .map(|t| match t {
                    "x" => Ok(96),
                    "X" => Ok(122),
                    "o" => Ok(52),
                    "." => Ok(0),
                    _ => Err(format!("drum '{name}': '{t}' (use x X o .)")),
                })
                .collect::<Result<_, _>>()?;
            if steps != 0 && hits.len() != steps {
                return Err(format!("drum '{name}' has {} steps, the others {steps}", hits.len()));
            }
            steps = hits.len();
            parsed.push((key, hits));
        }
        if steps == 0 {
            return Err("empty drum pattern".into());
        }
        Ok(Self { bpm, step, steps, lanes: parsed })
    }

    pub fn step_secs(&self) -> f64 {
        step_secs(self.bpm, self.step)
    }

    /// `(seconds, key, velocity)` from 0 to `until`, looped.
    pub fn hits(&self, until: f64) -> Vec<(f64, u8, u8)> {
        let dt = self.step_secs();
        let mut out = Vec::new();
        let mut base = 0.0;
        while base < until {
            for i in 0..self.steps {
                let t = base + i as f64 * dt;
                if t >= until {
                    break;
                }
                for (key, hits) in &self.lanes {
                    if hits[i] > 0 {
                        out.push((t, *key, hits[i]));
                    }
                }
            }
            base += self.steps as f64 * dt;
        }
        out
    }

    pub fn track(&self, bars: usize) -> ScoreTrack {
        let step_ticks = TICKS_PER_QUARTER * 4 / self.step as i64;
        let total = bars as i64 * TICKS_PER_QUARTER * 4;
        let mut beats = Vec::new();
        let mut base = 0;
        while base < total {
            for i in 0..self.steps {
                let notes: Vec<TabNote> = self
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, hits))| hits[i] > 0)
                    .map(|(lane, (key, hits))| TabNote {
                        string: (lane % 6 + 1) as u8,
                        fret: *key as i16,
                        velocity: hits[i],
                        ..TabNote::default()
                    })
                    .collect();
                beats.push((base + i as i64 * step_ticks, step_ticks, notes));
            }
            base += self.steps as i64 * step_ticks;
        }
        ScoreTrack {
            name: "Drums".into(),
            tuning: vec![0; 6],
            percussion: true,
            volume: 100,
            bars: bar_beats(&beats, bars),
            ..ScoreTrack::default()
        }
    }
}

/// `(tick, length, notes)` beats laid into `bars` 4/4 bars: split at bar
/// lines into writable durations (tied), gaps filled with rests.
fn bar_beats(beats: &[(i64, i64, Vec<TabNote>)], bars: usize) -> Vec<TrackBar> {
    let bar_ticks = TICKS_PER_QUARTER * 4;
    let total = bars as i64 * bar_ticks;
    let mut out: Vec<TrackBar> = (0..bars).map(|_| TrackBar { voices: vec![Vec::new()] }).collect();
    let mut push = |tick: i64, len: i64, notes: &[TabNote]| {
        let mut tick = tick;
        let mut left = len.min(total - tick);
        let mut first = true;
        while left > 0 {
            let bar = (tick / bar_ticks) as usize;
            let in_bar = tick % bar_ticks;
            let (value, dotted, ticks) = writable((bar_ticks - in_bar).min(left));
            out[bar].voices[0].push(TabBeat {
                start: in_bar,
                duration: ticks,
                value,
                dotted,
                palm_mute: notes.iter().any(|n| n.palm_mute),
                let_ring: notes.iter().any(|n| n.let_ring),
                notes: notes.iter().map(|n| TabNote { tie: !first, ..n.clone() }).collect(),
                ..TabBeat::default()
            });
            first = false;
            tick += ticks;
            left -= ticks;
        }
    };
    let mut at = 0;
    for (tick, len, notes) in beats {
        if *tick >= total {
            break;
        }
        if *tick > at {
            push(at, tick - at, &[]);
        }
        push(*tick, *len, notes);
        at = tick + len;
    }
    if at < total {
        push(at, total - at, &[]);
    }
    out
}

/// The longest note value (and dot) not longer than `ticks`:
/// `(value, dotted, its ticks)`.
fn writable(ticks: i64) -> (u16, bool, i64) {
    let whole = TICKS_PER_QUARTER * 4;
    for value in [1u16, 2, 4, 8, 16, 32] {
        let plain = whole / value as i64;
        if plain * 3 / 2 <= ticks {
            return (value, true, plain * 3 / 2);
        }
        if plain <= ticks {
            return (value, false, plain);
        }
    }
    (32, false, whole / 32)
}

/// A score of `bars` 4/4 bars with these tracks.
pub fn score(title: &str, artist: &str, bpm: f64, bars: usize, tracks: Vec<ScoreTrack>) -> Score {
    Score {
        title: title.into(),
        artist: artist.into(),
        initial_tempo: bpm,
        bars: (0..bars)
            .map(|i| MasterBar {
                numerator: 4,
                denominator: 4,
                tempo_changes: if i == 0 { vec![(0, bpm)] } else { Vec::new() },
                ..MasterBar::default()
            })
            .collect(),
        tracks,
        ..Score::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_holds_rests_chords_and_suffixes() {
        let r = Riff::parse(120.0, 8, "6:0 - . 6:3+5:5m | 1:12~!", &GUITAR, false).unwrap();
        assert_eq!(r.steps, 5);
        assert_eq!(r.events[0].len, 2);
        assert!(r.events[1].notes.is_empty());
        assert_eq!(r.events[2].notes, vec![Hit { string: 6, fret: 3 }, Hit { string: 5, fret: 5 }]);
        assert!(r.events[2].mute && !r.events[2].vibrato);
        assert!(r.events[3].vibrato && r.events[3].accent);
        assert!(Riff::parse(120.0, 8, "- 6:0", &GUITAR, false).is_err());
        assert!(Riff::parse(120.0, 8, "5:0", &BASS, false).is_err());
    }

    #[test]
    fn ring_holds_until_the_same_string_plays_again() {
        let r = Riff::parse(120.0, 8, "6:0 4:2 6:3 .", &GUITAR, true).unwrap();
        let p = r.played(r.secs());
        assert!((p[0].end - p[2].start).abs() < 1e-9);
        assert!(p[1].end > p[2].start, "string 4 keeps ringing");
    }

    #[test]
    fn tracks_fill_their_bars_and_survive_gp5() {
        let r = Riff::parse(100.0, 8, "6:0 6:0 6:0 6:0 6:0 6:0 6:0 5:2 - - - - - 4:2", &GUITAR, false).unwrap();
        let lanes = [("kick".to_string(), "x . . . x . . .".to_string()), ("snare".to_string(), ". . x . . . x .".to_string())];
        let d = Drums::parse(100.0, 8, &lanes).unwrap();
        assert_eq!(d.hits(d.step_secs() * 8.0).len(), 4);
        let s = score("T", "A", 100.0, 3, vec![r.track("Guitar", 29, 3), d.track(3)]);
        for track in &s.tracks {
            for bar in &track.bars {
                let sum: i64 = bar.voices[0].iter().map(|b| b.duration).sum();
                assert_eq!(sum, TICKS_PER_QUARTER * 4);
            }
        }
        assert!(s.tracks[0].bars[1].voices[0][0].notes[0].tie, "the held note continues into bar 2");
        assert!(guitar_core::shared::tab::export::score_to_gp5(&s).unwrap().len() > 100);
    }
}
