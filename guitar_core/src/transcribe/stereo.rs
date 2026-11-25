//! Splits a stereo guitar stem by pan position, so guitars mixed to
//! different sides come out as separate parts: rock rhythm guitars are
//! usually double-tracked hard left and right, with the lead in the middle.
//! This is the azimuth idea of ADRess (Barry, Lawlor and Coyle, "Sound
//! source separation: azimuth discrimination and resynthesis", DAFx 2004)
//! with soft masks: every STFT bin goes to the left, center or right part
//! by the level difference between the channels, which is all a pan pot
//! changes.
//!
//! The parts are only candidates: one guitar panned halfway lands partly
//! in two of them, and a double-tracked part plays the same notes on both
//! sides. [`super::guitar_parts`] compares their transcriptions to decide
//! how many guitars there really are.

use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

const FFT_SIZE: usize = 2048;
const HOP: usize = FFT_SIZE / 4;
/// Pan positions (-1 = hard left, 1 = hard right) up to this far from the
/// middle are the center part; beyond [`SIDE`] they are fully a side part,
/// with a linear crossfade in between.
const CENTER: f32 = 0.2;
const SIDE: f32 = 0.5;

/// Where a part sits in the stereo image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Center,
    Right,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Left => "esquerda",
            Side::Center => "centro",
            Side::Right => "direita",
        }
    }
}

/// One pan region of the stem, as mono audio.
#[derive(Clone, Debug)]
pub struct PanPart {
    pub side: Side,
    pub samples: Vec<f32>,
    /// Share of the stem's energy in this part, 0..1.
    pub share: f32,
}

/// Weights of the left, center and right parts for a bin at pan `x`.
fn weights(x: f32) -> [f32; 3] {
    let side = ((x.abs() - CENTER) / (SIDE - CENTER)).clamp(0.0, 1.0);
    if x < 0.0 {
        [side, 1.0 - side, 0.0]
    } else {
        [0.0, 1.0 - side, side]
    }
}

/// Pan of a bin from its channel magnitudes: -1 = only left, 1 = only right.
fn pan(l: f32, r: f32) -> f32 {
    let total = l + r;
    if total <= 1e-12 { 0.0 } else { (r - l) / total }
}

struct Stft {
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
}

impl Stft {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            forward: planner.plan_fft_forward(FFT_SIZE),
            inverse: planner.plan_fft_inverse(FFT_SIZE),
            window,
        }
    }

    fn frame(&self, x: &[f32], start: isize) -> Vec<Complex<f32>> {
        let mut buf: Vec<Complex<f32>> = (0..FFT_SIZE)
            .map(|i| {
                let j = start + i as isize;
                let v = if j >= 0 {
                    x.get(j as usize).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                Complex::new(v * self.window[i], 0.0)
            })
            .collect();
        self.forward.process(&mut buf);
        buf
    }
}

/// Splits `left`/`right` into left, center and right parts (mono, same
/// length and rate as the input). Perfect reconstruction: the parts add up
/// to the mid signal `(left + right) / 2`.
pub fn split_by_pan(left: &[f32], right: &[f32]) -> Vec<PanPart> {
    let n = left.len().min(right.len());
    let stft = Stft::new();
    let mut out = vec![vec![0f32; n]; 3];
    let mut norm = vec![0f32; n];
    let bins = FFT_SIZE / 2 + 1;
    let mut start = -((FFT_SIZE - HOP) as isize);
    while start < n as isize {
        let l = stft.frame(&left[..n], start);
        let r = stft.frame(&right[..n], start);
        for (part, signal) in out.iter_mut().enumerate() {
            let mut spec = vec![Complex::new(0.0, 0.0); FFT_SIZE];
            for k in 0..bins {
                let w = weights(pan(l[k].norm(), r[k].norm()))[part];
                spec[k] = (l[k] + r[k]) * (0.5 * w);
                if k > 0 && k < FFT_SIZE / 2 {
                    spec[FFT_SIZE - k] = spec[k].conj();
                }
            }
            stft.inverse.process(&mut spec);
            for (i, v) in spec.iter().enumerate() {
                let j = start + i as isize;
                if (0..n as isize).contains(&j) {
                    signal[j as usize] += v.re / FFT_SIZE as f32 * stft.window[i];
                }
            }
        }
        for (i, w) in stft.window.iter().enumerate() {
            let j = start + i as isize;
            if (0..n as isize).contains(&j) {
                norm[j as usize] += w * w;
            }
        }
        start += HOP as isize;
    }
    for signal in &mut out {
        for (v, w) in signal.iter_mut().zip(&norm) {
            if *w > 1e-6 {
                *v /= w;
            }
        }
    }
    let energy: Vec<f64> = out
        .iter()
        .map(|s| s.iter().map(|v| (*v as f64).powi(2)).sum())
        .collect();
    let total = energy.iter().sum::<f64>().max(1e-12);
    [Side::Left, Side::Center, Side::Right]
        .into_iter()
        .zip(out)
        .zip(energy)
        .map(|((side, samples), e)| PanPart {
            side,
            samples,
            share: (e / total) as f32,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, n: usize, rate: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / rate).sin() * 0.5)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn panned_sources_land_in_their_part() {
        let n = 44_100;
        let a = sine(220.0, n, 44_100.0); // hard left
        let b = sine(660.0, n, 44_100.0); // center
        let c = sine(1500.0, n, 44_100.0); // hard right
        let left: Vec<f32> = (0..n).map(|i| a[i] + 0.5 * b[i]).collect();
        let right: Vec<f32> = (0..n).map(|i| c[i] + 0.5 * b[i]).collect();
        let parts = split_by_pan(&left, &right);
        assert_eq!(parts.len(), 3);
        let shares: Vec<f32> = parts.iter().map(|p| p.share).collect();
        assert!(shares.iter().all(|s| *s > 0.1), "{shares:?}");
        // Each part is mostly its own source: compare with the other two.
        let energy_at = |x: &[f32], f: f32| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, v) in x.iter().enumerate().skip(4096).take(32_768) {
                let ph = std::f32::consts::TAU * f * i as f32 / 44_100.0;
                re += v * ph.cos();
                im += v * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        for (part, own, others) in [
            (&parts[0], 220.0, [660.0, 1500.0]),
            (&parts[1], 660.0, [220.0, 1500.0]),
            (&parts[2], 1500.0, [220.0, 660.0]),
        ] {
            let mine = energy_at(&part.samples, own);
            for o in others {
                assert!(
                    mine > 20.0 * energy_at(&part.samples, o),
                    "{:?}: {own} Hz vs {o} Hz",
                    part.side
                );
            }
        }
    }

    #[test]
    fn parts_add_up_to_the_mid_signal() {
        let n = 20_000;
        let left = sine(330.0, n, 44_100.0);
        let right: Vec<f32> = sine(500.0, n, 44_100.0)
            .iter()
            .zip(&left)
            .map(|(a, b)| a + 0.3 * b)
            .collect();
        let parts = split_by_pan(&left, &right);
        let sum: Vec<f32> = (0..n)
            .map(|i| parts.iter().map(|p| p.samples[i]).sum())
            .collect();
        let mid: Vec<f32> = (0..n).map(|i| 0.5 * (left[i] + right[i])).collect();
        let err: Vec<f32> = sum.iter().zip(&mid).map(|(a, b)| a - b).collect();
        assert!(rms(&err) < 1e-3 * rms(&mid), "error {}", rms(&err));
    }

    #[test]
    fn a_mono_signal_is_all_center() {
        let x = sine(440.0, 10_000, 44_100.0);
        let parts = split_by_pan(&x, &x);
        assert!(
            parts[1].share > 0.999,
            "{:?}",
            parts.iter().map(|p| p.share).collect::<Vec<_>>()
        );
    }
}
