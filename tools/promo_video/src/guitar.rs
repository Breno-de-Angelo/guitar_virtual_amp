//! The guitar going into the app: real DI recordings of single notes
//! (Guitar-TECHS "allsinglenotes", CC BY 4.0: every fret of every string,
//! by two players), cut per string and fret and played back as a riff with
//! strummed chords, palm mutes, vibrato and a player's small timing and
//! velocity variations. Each player is one "take", so a part can be double
//! tracked with two different guitars. Without the recordings it falls back
//! to the app's plucked-string model (`tone::pluck`), which sounds plainer.

use std::collections::HashMap;
use std::path::Path;

use guitar_core::tone::pluck;

use crate::riff::{GUITAR, Played};

pub const RATE: usize = 48_000;
/// Recorded audio kept before the labelled onset, so the attack is whole.
const PRE_ROLL: f64 = 0.008;
/// A released note is damped to silence in this time.
const RELEASE: f64 = 0.07;
/// DI level: a strummed chord averages about -20 dBFS and peaks near
/// -6 dBFS, what a humbucker gives an interface with the gain set sensibly
/// (and what `backend::offline::render` assumes).
const NOTE_RMS: f32 = 0.08;

struct Take {
    /// (string, fret) -> recorded note, RMS-normalized over its attack, and
    /// the read speed that puts it in tune at A440.
    notes: HashMap<(u8, u8), (Vec<f32>, f64)>,
}

/// Read speed that brings `note` to `pitch` exactly: its period found by
/// autocorrelation near the expected one, after the attack settles.
fn intonation(note: &[f32], pitch: u8) -> f64 {
    let expected = 440.0 * 2f64.powf((pitch as f64 - 69.0) / 12.0);
    let period = RATE as f64 / expected;
    let (a, b) = (RATE * 3 / 10, (RATE * 8 / 10).min(note.len()));
    let lo = (period * 0.95) as usize;
    let hi = (period * 1.05) as usize + 1;
    if b <= a + hi * 4 {
        return 1.0;
    }
    let w = &note[a..b];
    let r = |lag: usize| -> f64 {
        let n = w.len() - lag;
        let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
        for i in 0..n {
            let (x, y) = (w[i] as f64, w[i + lag] as f64);
            xy += x * y;
            xx += x * x;
            yy += y * y;
        }
        xy / (xx * yy).sqrt().max(1e-12)
    };
    let scores: Vec<f64> = (lo - 1..=hi + 1).map(r).collect();
    let best = (1..scores.len() - 1).max_by(|&i, &j| scores[i].total_cmp(&scores[j])).unwrap();
    if scores[best] < 0.8 {
        return 1.0;
    }
    // Parabolic interpolation around the peak.
    let (y0, y1, y2) = (scores[best - 1], scores[best], scores[best + 1]);
    let shift = 0.5 * (y0 - y2) / (y0 - 2.0 * y1 + y2).min(-1e-12);
    let lag = (lo - 1 + best) as f64 + shift;
    // Played back at `speed`, the period becomes lag / speed.
    let speed = lag / period;
    if (speed.ln() / 2f64.ln() * 1200.0).abs() > 40.0 { 1.0 } else { speed }
}

pub struct Guitar {
    takes: Vec<Take>,
}

/// A tiny deterministic random source (xorshift), for humanizing.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    /// -1..1.
    pub fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let audio = guitar_core::backend::audio_file::decode_file(path)?;
    Ok(guitar_core::transcribe::resample::resample(&audio.samples, audio.sample_rate, RATE as u32))
}

impl Take {
    /// `wav` is the DI recording, `notes` its labels (`onset offset pitch
    /// velocity` per line), played string by string from the low E, each
    /// string from its lowest fret up.
    fn load(wav: &Path, notes: &Path, brightness_db: f32) -> Result<Self, String> {
        let mut audio = read_wav(wav)?;
        // These pickups were recorded dark; a brighter guitar sounds closer
        // to what players plug in.
        crate::dsp::Biquad::high_shelf(2500.0, brightness_db).run(&mut audio);
        crate::dsp::Biquad::high_shelf(5000.0, 8.0).run(&mut audio);
        let text = std::fs::read_to_string(notes).map_err(|e| format!("{}: {e}", notes.display()))?;
        let mut labels: Vec<(f64, u8)> = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let on: f64 = it.next()?.parse().ok()?;
                let _off: f64 = it.next()?.parse().ok()?;
                let pitch: f64 = it.next()?.parse().ok()?;
                Some((on, pitch.round() as u8))
            })
            .collect();
        labels.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out = HashMap::new();
        let mut string = 6usize;
        let mut prev: Option<u8> = None;
        for (i, &(on, pitch)) in labels.iter().enumerate() {
            // The pitch drops when the player moves to the next string.
            if prev.is_some_and(|p| pitch + 3 < p) {
                string -= 1;
                if string == 0 {
                    break;
                }
            }
            prev = Some(pitch);
            let open = GUITAR[string - 1];
            if pitch < open {
                continue;
            }
            let fret = pitch - open;
            let next = labels.get(i + 1).map_or(on + 3.0, |n| n.0);
            let start = ((on - PRE_ROLL).max(0.0) * RATE as f64) as usize;
            let end = (((next - 0.02) * RATE as f64) as usize).min(audio.len());
            if end <= start + RATE / 4 || out.contains_key(&(string as u8, fret)) {
                continue;
            }
            let mut note = audio[start..end].to_vec();
            // The next label cuts the recording while the string still rings.
            let fade = (0.15 * RATE as f64) as usize;
            let n = note.len();
            for (k, s) in note.iter_mut().skip(n - fade.min(n)).enumerate() {
                *s *= 1.0 - k as f32 / fade as f32;
            }
            let attack = &note[..note.len().min(RATE / 3)];
            let rms = (attack.iter().map(|s| s * s).sum::<f32>() / attack.len() as f32).sqrt().max(1e-6);
            note.iter_mut().for_each(|s| *s *= NOTE_RMS / rms);
            let tune = intonation(&note, pitch);
            out.insert((string as u8, fret), (note, tune));
        }
        if out.len() < 20 {
            return Err(format!("{}: only {} notes found", wav.display(), out.len()));
        }
        Ok(Self { notes: out })
    }

    /// The recording for (string, fret), or the nearest one re-pitched:
    /// `(audio, read speed)`.
    fn sample(&self, string: u8, fret: u8) -> (&[f32], f64) {
        let pitch = GUITAR[string as usize - 1] + fret;
        let best = self
            .notes
            .iter()
            .min_by_key(|((s, f), _)| {
                let p = GUITAR[*s as usize - 1] + f;
                // Same string first, then the closest pitch.
                (p as i32 - pitch as i32).abs() * 4 + if *s == string { 0 } else { 3 }
            })
            .expect("loaded takes have notes");
        let p = GUITAR[best.0.0 as usize - 1] + best.0.1;
        (&best.1.0, best.1.1 * 2f64.powf((pitch as f64 - p as f64) / 12.0))
    }
}

impl Guitar {
    /// `specs` are `"di.wav:notes.txt"`, one per take.
    pub fn load(specs: &[String]) -> Result<Self, String> {
        let mut takes = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            let (wav, notes) = spec.split_once(':').ok_or_else(|| format!("'{spec}': expected di.wav:notes.txt"))?;
            // Both recordings are dark next to a typical DI (EGDB's: about
            // 18 dB less at 4-8 kHz), player 2's 5-10 dB more than player 1's.
            let brightness = if i == 1 { 15.0 } else { 10.0 };
            takes.push(Take::load(&crate::storyboard::expand_home(wav), &crate::storyboard::expand_home(notes), brightness)?);
        }
        Ok(Self { takes })
    }

    /// The plucked-string fallback.
    pub fn synthetic() -> Self {
        Self { takes: Vec::new() }
    }

    pub fn is_sampled(&self) -> bool {
        !self.takes.is_empty()
    }

    /// Plays `notes` (riff seconds, shifted by `offset`) into `len` samples
    /// with take `take` (wraps around). Chords are strummed: down on the
    /// beat, up off it, as a player alternates.
    pub fn render(&self, take: usize, notes: &[Played], offset: f64, len: usize, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        // Humanize once per strum, then spread the strings.
        let mut groups: Vec<Vec<&Played>> = Vec::new();
        for n in notes {
            match groups.last_mut() {
                Some(g) if (g[0].start - n.start).abs() < 1e-6 => g.push(n),
                _ => groups.push(vec![n]),
            }
        }
        if self.takes.is_empty() {
            let mut plucks = Vec::new();
            for g in &groups {
                let jitter = rng.next() * 0.006;
                for (k, n) in g.iter().enumerate() {
                    let start = offset + n.start + jitter + k as f64 * 0.01;
                    plucks.push(pluck::Note { start, end: offset + n.end, pitch: n.pitch, velocity: n.velocity });
                }
            }
            let mut out = pluck::render(&plucks, len, RATE as u32);
            out.iter_mut().for_each(|s| *s *= 0.25);
            return out;
        }
        let t = &self.takes[take % self.takes.len()];
        let mut out = vec![0f32; len];
        for g in groups {
            let jitter = rng.next() * 0.007;
            let vel = (1.0 + rng.next() * 0.08) as f32;
            // Upstrokes hit the high strings first, lighter.
            let up = g.len() > 1 && !g[0].down;
            let mut strings: Vec<&Played> = g.clone();
            strings.sort_by_key(|n| std::cmp::Reverse(n.string));
            if up {
                strings.reverse();
            }
            let spread = if up { 0.006 } else { 0.011 };
            for (k, n) in strings.iter().enumerate() {
                let start = offset + n.start + jitter + k as f64 * spread;
                let v = n.velocity * vel * if up { 0.85 } else { 1.0 } * (1.0 - 0.04 * k as f32);
                note(t, n, start, offset + n.end + jitter, v, &mut out);
            }
        }
        out
    }

    /// One string ringing, re-plucked every `every` seconds, its pitch
    /// gliding from `cents.0` to `cents.1` over `glide` seconds: a guitar
    /// being tuned.
    pub fn tuning(&self, take: usize, string: u8, fret: u8, cents: (f32, f32), glide: f64, every: f64, len: usize) -> Vec<f32> {
        let mut out = vec![0f32; len];
        let (src, speed): (Vec<f32>, f64) = match self.takes.get(take % self.takes.len().max(1)) {
            Some(t) => {
                let (audio, speed) = t.sample(string, fret);
                (audio.to_vec(), speed)
            }
            None => {
                let note = pluck::Note { start: 0.0, end: every, pitch: GUITAR[string as usize - 1] + fret, velocity: 0.8 };
                (pluck::render(&[note], (every * 1.5 * RATE as f64) as usize, RATE as u32), 1.0)
            }
        };
        let mut t0 = 0.0;
        while t0 < len as f64 / RATE as f64 {
            let first = (t0 * RATE as f64) as usize;
            let mut phase = 0.0f64;
            for i in 0..((every * RATE as f64) as usize) {
                let j = first + i;
                if j >= len {
                    break;
                }
                let t = j as f64 / RATE as f64;
                let k = 1.0 - (1.0 - (t / glide).clamp(0.0, 1.0)).powi(3);
                let c = cents.0 as f64 + (cents.1 - cents.0) as f64 * k;
                let idx = phase as usize;
                if idx + 1 >= src.len() {
                    break;
                }
                let frac = (phase - idx as f64) as f32;
                let fade = ((every * RATE as f64) as usize - i).min(2400) as f32 / 2400.0;
                out[j] += (src[idx] + (src[idx + 1] - src[idx]) * frac) * fade * 0.9;
                phase += speed * 2f64.powf(c / 1200.0);
            }
            t0 += every;
        }
        out
    }
}

/// Adds one recorded note to `out`.
fn note(take: &Take, n: &Played, start: f64, end: f64, velocity: f32, out: &mut [f32]) {
    let (src, speed) = take.sample(n.string, n.fret);
    let gain = velocity.clamp(0.05, 1.2).powf(1.4);
    let first = ((start - PRE_ROLL).max(0.0) * RATE as f64) as usize;
    let held = if n.mute { (end - start).min(0.22) } else { end - start };
    let total = ((held + RELEASE + PRE_ROLL) * RATE as f64) as usize;
    let release_at = ((held + PRE_ROLL) * RATE as f64) as usize;
    // Palm mute: the hand on the bridge darkens the string and kills it
    // fast; the pick attack stays.
    let mute_tau = 0.09 * RATE as f32;
    let mut lp = 0.0f32;
    let lp_k = 1.0 - (-2.0 * std::f32::consts::PI * 900.0 / RATE as f32).exp();
    let mut phase = 0.0f64;
    for i in 0..total {
        let j = first + i;
        if j >= out.len() {
            break;
        }
        let idx = phase as usize;
        if idx + 1 >= src.len() {
            break;
        }
        let frac = (phase - idx as f64) as f32;
        let mut v = src[idx] + (src[idx + 1] - src[idx]) * frac;
        if n.mute {
            lp += lp_k * (v - lp);
            let attack = (-(i as f32) / (0.012 * RATE as f32)).exp();
            v = (lp * 1.6 * (1.0 - attack) + v * attack) * (-(i as f32) / mute_tau).exp().max(0.0) * 1.15;
        }
        if i > release_at {
            v *= (-((i - release_at) as f32) / (0.018 * RATE as f32)).exp();
        }
        out[j] += v * gain;
        let mut rate = speed;
        if n.vibrato {
            // Wide, even vibrato after the note settles, like a held lead note.
            let t = i as f64 / RATE as f64;
            let depth = ((t - 0.25) / 0.35).clamp(0.0, 1.0) * 28.0;
            rate *= 2f64.powf(depth * (std::f64::consts::TAU * 5.2 * t).sin() / 1200.0);
        }
        phase += rate;
    }
}
