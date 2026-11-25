//! Tempo, beat and downbeat tracking, so transcribed notes can be placed
//! in bars and beats.
//!
//! - Onset strength: spectral flux of the log-magnitude STFT (whole band,
//!   plus a low band under ~200 Hz where kick and bass mark the downbeats).
//! - Tempo: autocorrelation of the onset strength, weighted by a log-normal
//!   prior around 120 BPM (as in librosa).
//! - Beats: dynamic programming that rewards onsets on beats and penalizes
//!   tempo deviations (Ellis, "Beat Tracking by Dynamic Programming", 2007).
//! - Downbeats: the beat phase (in 4/4) with the most low-band onset energy.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Rate of the audio given to [`onset_envelope`] by the pipeline.
pub const SAMPLE_RATE: u32 = 22_050;
const N_FFT: usize = 2048;
pub const HOP: usize = 256;
const LOW_BAND_HZ: f64 = 200.0;
const TIGHTNESS: f64 = 100.0;
pub const BEATS_PER_BAR: usize = 4;
/// 16th notes per beat (the quantization grid in `arrange`).
const STEPS_PER_BEAT_F: f64 = 4.0;

#[derive(Clone, Debug, Default)]
pub struct OnsetEnvelope {
    /// Frames per second.
    pub rate: f64,
    pub full: Vec<f32>,
    pub low: Vec<f32>,
}

fn normalize(values: &mut [f32]) {
    let n = values.len().max(1) as f32;
    let mean = values.iter().sum::<f32>() / n;
    let std = (values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n)
        .sqrt()
        .max(1e-6);
    for v in values.iter_mut() {
        *v /= std;
    }
}

pub fn onset_envelope(audio: &[f32], sample_rate: u32) -> OnsetEnvelope {
    let rate = sample_rate as f64 / HOP as f64;
    if audio.len() < N_FFT {
        return OnsetEnvelope {
            rate,
            ..Default::default()
        };
    }
    let fft: Arc<dyn Fft<f32>> = FftPlanner::new().plan_fft_forward(N_FFT);
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N_FFT as f32).cos())
        .collect();
    let bins = N_FFT / 2 + 1;
    let low_bins =
        ((LOW_BAND_HZ * N_FFT as f64 / sample_rate as f64).ceil() as usize).clamp(2, bins);
    let frames = (audio.len() - N_FFT) / HOP + 1;
    let mut previous = vec![0f32; bins];
    let mut buffer = vec![Complex::new(0f32, 0f32); N_FFT];
    let (mut full, mut low) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    for f in 0..frames {
        let start = f * HOP;
        for (i, slot) in buffer.iter_mut().enumerate() {
            *slot = Complex::new(audio[start + i] * window[i], 0.0);
        }
        fft.process(&mut buffer);
        let (mut flux, mut low_flux) = (0f32, 0f32);
        for (b, prev) in previous.iter_mut().enumerate() {
            let mag = (1.0 + 1000.0 * buffer[b].norm()).ln();
            let rise = (mag - *prev).max(0.0);
            flux += rise;
            if b < low_bins {
                low_flux += rise;
            }
            *prev = mag;
        }
        full.push(if f == 0 { 0.0 } else { flux });
        low.push(if f == 0 { 0.0 } else { low_flux });
    }
    // Frame f covers samples f*HOP..f*HOP+N_FFT. An onset raises the flux
    // most once it's about 3/4 into the Hann window, so shift by that much
    // to make index = onset time.
    let shift = N_FFT * 3 / 4 / HOP;
    let mut full: Vec<f32> = std::iter::repeat_n(0.0, shift).chain(full).collect();
    let mut low: Vec<f32> = std::iter::repeat_n(0.0, shift).chain(low).collect();
    normalize(&mut full);
    normalize(&mut low);
    OnsetEnvelope { rate, full, low }
}

/// Tempo in BPM, in 40..=240.
pub fn estimate_tempo(env: &OnsetEnvelope) -> f64 {
    let rate = env.rate;
    let min_lag = (rate * 60.0 / 240.0).floor().max(1.0) as usize;
    let max_lag = ((rate * 60.0 / 40.0).ceil() as usize).min(env.full.len().saturating_sub(1));
    if max_lag <= min_lag + 2 {
        return 120.0;
    }
    let x = &env.full;
    let mean = x.iter().sum::<f32>() / x.len() as f32;
    let centered: Vec<f32> = x.iter().map(|v| v - mean).collect();
    let score = |lag: usize| -> f64 {
        let ac: f64 = centered
            .iter()
            .zip(&centered[lag..])
            .map(|(a, b)| (*a as f64) * (*b as f64))
            .sum();
        let bpm = rate * 60.0 / lag as f64;
        let prior = (-0.5 * ((bpm / 120.0).log2() / 1.0).powi(2)).exp();
        ac / (x.len() - lag) as f64 * prior
    };
    let scores: Vec<f64> = (min_lag..=max_lag).map(score).collect();
    let (best, _) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap_or((0, &0.0));
    // Parabolic interpolation for a fractional lag.
    let lag = if best > 0 && best + 1 < scores.len() {
        let (a, b, c) = (scores[best - 1], scores[best], scores[best + 1]);
        let denom = a - 2.0 * b + c;
        let offset = if denom.abs() > 1e-12 {
            0.5 * (a - c) / denom
        } else {
            0.0
        };
        (min_lag + best) as f64 + offset.clamp(-0.5, 0.5)
    } else {
        (min_lag + best) as f64
    };
    (rate * 60.0 / lag).clamp(40.0, 240.0)
}

/// Beat frames (indices into the envelope) for a tempo of `bpm`.
pub fn track_beats(env: &OnsetEnvelope, bpm: f64) -> Vec<usize> {
    let n = env.full.len();
    let period = env.rate * 60.0 / bpm;
    if n < 4 || period < 2.0 {
        return Vec::new();
    }
    // Local score: onset strength smoothed with a Gaussian ~period/32 wide.
    let radius = period.round() as isize;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 * 32.0 / period).powi(2)).exp())
        .collect();
    let local: Vec<f64> = (0..n as isize)
        .map(|i| {
            kernel
                .iter()
                .enumerate()
                .map(|(k, w)| {
                    let j = i + k as isize - radius;
                    if (0..n as isize).contains(&j) {
                        w * env.full[j as usize] as f64
                    } else {
                        0.0
                    }
                })
                .sum()
        })
        .collect();

    let mut cumulative = vec![0f64; n];
    let mut backlink = vec![usize::MAX; n];
    let (min_gap, max_gap) = (
        (period / 2.0).round() as usize,
        (2.0 * period).round() as usize,
    );
    for i in 0..n {
        let mut best = f64::MIN;
        let mut best_j = usize::MAX;
        let lo = i.saturating_sub(max_gap);
        let hi = i.saturating_sub(min_gap.max(1));
        if i >= min_gap.max(1) {
            for (j, previous) in cumulative.iter().enumerate().take(hi + 1).skip(lo) {
                let gap = (i - j) as f64;
                let value = previous - TIGHTNESS * (gap / period).ln().powi(2);
                if value > best {
                    best = value;
                    best_j = j;
                }
            }
        }
        cumulative[i] = local[i] + if best_j == usize::MAX { 0.0 } else { best };
        backlink[i] = best_j;
    }

    // Last beat: the best cumulative score within the last period.
    let tail_start = n.saturating_sub(period.ceil() as usize + 1);
    let mut i = (tail_start..n)
        .max_by(|a, b| cumulative[*a].total_cmp(&cumulative[*b]))
        .unwrap_or(n - 1);
    let mut beats = vec![i];
    while backlink[i] != usize::MAX {
        i = backlink[i];
        beats.push(i);
    }
    beats.reverse();

    // Drop weak beats at the edges (silence before/after the music).
    let rms = (local.iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt();
    let threshold = 0.5 * rms;
    let first = beats
        .iter()
        .position(|&b| local[b] >= threshold)
        .unwrap_or(0);
    let last = beats
        .iter()
        .rposition(|&b| local[b] >= threshold)
        .unwrap_or(beats.len() - 1);
    beats[first..=last.max(first)].to_vec()
}

/// Which beat (0..BEATS_PER_BAR) of `beats` starts a bar: the phase with
/// the most low-band onset energy around its beats.
pub fn downbeat_phase(env: &OnsetEnvelope, beats: &[usize]) -> usize {
    let energy = |frame: usize| -> f32 {
        let lo = frame.saturating_sub(2);
        let hi = (frame + 2).min(env.low.len().saturating_sub(1));
        (lo..=hi)
            .filter_map(|f| env.low.get(f))
            .fold(0.0, |m, v| m.max(*v))
    };
    (0..BEATS_PER_BAR)
        .max_by(|a, b| {
            let sum = |phase: usize| -> f32 {
                beats
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % BEATS_PER_BAR == phase)
                    .map(|(_, f)| energy(*f))
                    .sum()
            };
            sum(*a).total_cmp(&sum(*b))
        })
        .unwrap_or(0)
}

/// Downbeat indices every [`BEATS_PER_BAR`] beats from `phase` (the DSP
/// tracker's fixed 4/4).
pub fn regular_downbeats(beats: usize, phase: usize) -> Vec<usize> {
    (phase.min(beats.saturating_sub(1))..beats)
        .step_by(BEATS_PER_BAR)
        .collect()
}

/// Whole bars covering `first_note..last_note`: `(beat times, beats per
/// bar)`, where `times[0]` is a downbeat and the bar lengths add up to
/// `times.len() - 1` (the last time closes the last bar). Bars come from
/// `downbeats` (indices into `beat_times`); the grid is extended before the
/// first downbeat with bars like the first one and after the last beat
/// with bars like the last one, at the local beat period.
pub fn bar_grid(
    beat_times: &[f64],
    downbeats: &[usize],
    first_note: f64,
    last_note: f64,
    fallback_bpm: f64,
) -> (Vec<f64>, Vec<usize>) {
    let first_down = downbeats
        .first()
        .copied()
        .filter(|&d| d + 1 < beat_times.len());
    let (mut beats, mut lengths): (Vec<f64>, Vec<usize>) = match first_down {
        Some(d0) => {
            let beats = beat_times[d0..].to_vec();
            let mut lengths: Vec<usize> = downbeats
                .windows(2)
                .map(|w| w[1] - w[0])
                .filter(|&l| l > 0)
                .collect();
            // Beats after the last downbeat: a bar like the one before.
            let rest = beats.len() - 1 - lengths.iter().sum::<usize>();
            if rest > 0 || lengths.is_empty() {
                lengths.push(rest.max(1));
            }
            (beats, lengths)
        }
        None => {
            let period = 60.0 / fallback_bpm.max(1.0);
            let start = first_note.max(0.0);
            (vec![start, start + period], vec![1])
        }
    };
    let typical = |l: &[usize], i: usize| l.get(i).copied().unwrap_or(BEATS_PER_BAR).max(1);
    let first_period = beats[1] - beats[0];
    // Prepend whole bars until the grid starts before the first note (a
    // note a little ahead of the beat still belongs to it).
    let slack = 0.5 * first_period / STEPS_PER_BEAT_F;
    let first_len = if lengths.len() > 1 {
        typical(&lengths, 0)
    } else {
        BEATS_PER_BAR
    };
    while beats[0] > first_note + slack {
        let start = beats[0];
        let new: Vec<f64> = (1..=first_len)
            .rev()
            .map(|k| start - k as f64 * first_period)
            .collect();
        beats.splice(0..0, new);
        lengths.insert(0, first_len);
    }
    // Drop whole bars that end before the first note.
    while lengths.len() > 1 && beats[lengths[0]] <= first_note {
        beats.drain(0..lengths[0]);
        lengths.remove(0);
    }
    // The last bar may be short (beats ran out): complete it like the bar
    // before, then append bars past the last note.
    let n = beats.len();
    let last_period = (beats[n - 1] - beats[n - 2]).max(1e-3);
    let full = if lengths.len() > 1 {
        typical(&lengths, lengths.len() - 2)
    } else {
        BEATS_PER_BAR
    };
    let last = lengths.len() - 1;
    if lengths[last] < full {
        for _ in lengths[last]..full {
            let next = beats.last().unwrap() + last_period;
            beats.push(next);
        }
        lengths[last] = full;
    }
    while *beats.last().unwrap() < last_note {
        for _ in 0..full {
            let next = beats.last().unwrap() + last_period;
            beats.push(next);
        }
        lengths.push(full);
    }
    (beats, lengths)
}

/// Beat times in seconds of DSP-tracked beat frames.
pub fn frame_times(env: &OnsetEnvelope, frames: &[usize]) -> Vec<f64> {
    frames.iter().map(|&f| f as f64 / env.rate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 22_050;

    /// Click track: high clicks on every beat, a low thump on downbeats.
    fn clicks(bpm: f64, beats: usize, offset: f64) -> Vec<f32> {
        let period = 60.0 / bpm;
        let len = ((offset + beats as f64 * period + 1.0) * SR as f64) as usize;
        let mut out = vec![0f32; len];
        for b in 0..beats {
            let start = ((offset + b as f64 * period) * SR as f64) as usize;
            for i in 0..4000.min(len - start) {
                let t = i as f64 / SR as f64;
                let mut v = 0.3 * (std::f64::consts::TAU * 3000.0 * t).sin() * (-t * 60.0).exp();
                if b % 4 == 0 {
                    v += 0.8 * (std::f64::consts::TAU * 80.0 * t).sin() * (-t * 12.0).exp();
                }
                out[start + i] += v as f32;
            }
        }
        out
    }

    #[test]
    fn finds_tempo_beats_and_downbeats_of_a_click_track() {
        let offset = 0.37;
        let audio = clicks(100.0, 40, offset);
        let env = onset_envelope(&audio, SR);
        let bpm = estimate_tempo(&env);
        assert!((bpm - 100.0).abs() < 2.0, "tempo {bpm}");
        let beats = track_beats(&env, bpm);
        assert!(beats.len() >= 36, "{} beats", beats.len());
        // Each tracked beat is within 25 ms of a real click.
        for &f in &beats {
            let t = f as f64 / env.rate - offset;
            let nearest = (t / 0.6).round() * 0.6;
            assert!((t - nearest).abs() < 0.025, "beat at {t}");
        }
        let phase = downbeat_phase(&env, &beats);
        let first_down = beats[phase] as f64 / env.rate - offset;
        let index = (first_down / 0.6).round() as usize;
        assert_eq!(index % 4, 0, "downbeat at beat {index}");
    }

    #[test]
    fn grid_covers_the_notes_in_whole_bars() {
        let env = OnsetEnvelope {
            rate: 100.0,
            ..Default::default()
        };
        // Beats every 0.5 s from 2.0 s; notes from 0.3 s to 9.1 s.
        let frames: Vec<usize> = (0..12).map(|i| 200 + i * 50).collect();
        let downbeats = regular_downbeats(frames.len(), 0);
        let (grid, lengths) = bar_grid(&frame_times(&env, &frames), &downbeats, 0.3, 9.1, 120.0);
        assert!(
            grid[0] <= 0.3 && grid[0] > 0.3 - 2.0,
            "starts at {}",
            grid[0]
        );
        assert!((grid[4] - grid[0] - 2.0).abs() < 1e-9);
        assert!(*grid.last().unwrap() >= 9.1);
        assert!(lengths.iter().all(|&l| l == 4), "{lengths:?}");
        assert_eq!(lengths.iter().sum::<usize>(), grid.len() - 1);
        // The original beat at 2.0 s is still a downbeat.
        let idx = grid.iter().position(|t| (t - 2.0).abs() < 1e-9).unwrap();
        assert_eq!(idx % 4, 0);
    }

    #[test]
    fn grid_keeps_meter_changes() {
        // Beats every 0.5 s; bars of 4, 4, 2, 3, 3 beats, then 2 more beats.
        let times: Vec<f64> = (0..19).map(|i| 1.0 + i as f64 * 0.5).collect();
        let downbeats = [0, 4, 8, 10, 13, 16];
        let (grid, lengths) = bar_grid(&times, &downbeats, 1.0, 9.0, 120.0);
        // The trailing 2 beats are completed into a 3/4 bar like the last one.
        assert_eq!(lengths, vec![4, 4, 2, 3, 3, 3]);
        assert_eq!(grid.len(), 20);
        assert!((grid[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn silence_falls_back_to_a_steady_grid() {
        let env = onset_envelope(&vec![0.0; SR as usize * 3], SR);
        let (grid, lengths) = bar_grid(&frame_times(&env, &[]), &[], 1.0, 2.0, 120.0);
        assert!((grid[1] - grid[0] - 0.5).abs() < 1e-9);
        assert!(grid[0] <= 1.0);
        assert_eq!(lengths.iter().sum::<usize>(), grid.len() - 1);
        assert!(lengths.iter().all(|&l| l == 4), "{lengths:?}");
    }
}
