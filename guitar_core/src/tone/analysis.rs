//! Settings read off the signals before the search, where a search would
//! struggle (delay times make a rugged landscape: see the review, Section
//! IX).
//!
//! Delay: an echo repeats every onset of the target after the delay time.
//! The dry signal has the same notes without echoes, so a lag where the
//! target's onset envelope correlates with itself much more than the dry
//! signal's does is the delay time.

use rustfft::{FftPlanner, num_complex::Complex32};

const FFT: usize = 1024;
/// Onset envelope hop: 5 ms at 48 kHz.
const HOP: usize = 240;
/// Delay times considered, in seconds.
const MIN_DELAY: f64 = 0.06;
const MAX_DELAY: f64 = 1.2;
/// Smallest excess correlation (target minus dry) that counts as an echo:
/// the 95th percentile of the scores of 64 EGDB clips without delay (DI and
/// amp renders, pluck dry signal; `tone::eval::echo_scores`). With delays
/// of 150-700 ms at mix 0.15-0.5 it finds 24 of 32 at the right time.
pub const ECHO_THRESHOLD: f32 = 0.21;

/// Positive spectral flux of `x` (log magnitude), one value per [`HOP`].
fn onset_envelope(x: &[f32]) -> Vec<f32> {
    let window: Vec<f32> = (0..FFT)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos())
        .collect();
    let plan = FftPlanner::<f32>::new().plan_fft_forward(FFT);
    let frames = if x.len() >= FFT { (x.len() - FFT) / HOP + 1 } else { 0 };
    let mut prev = vec![0f32; FFT / 2];
    let mut out = Vec::with_capacity(frames);
    let mut buf = vec![Complex32::new(0.0, 0.0); FFT];
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
    for t in 0..frames {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex32::new(x[t * HOP + i] / peak * window[i], 0.0);
        }
        plan.process(&mut buf);
        let mut flux = 0.0;
        for (p, b) in prev.iter_mut().zip(&buf[..FFT / 2]) {
            let m = (1.0 + 100.0 * b.norm()).ln();
            flux += (m - *p).max(0.0);
            *p = m;
        }
        out.push(flux);
    }
    out
}

/// Normalised autocorrelation of `x` (mean removed) at `lag`.
fn autocorr(x: &[f32], lag: usize, energy: f32) -> f32 {
    if lag >= x.len() || energy <= 0.0 {
        return 0.0;
    }
    x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f32>() / energy
}

fn centred(x: Vec<f32>) -> (Vec<f32>, f32) {
    let mean = x.iter().sum::<f32>() / x.len().max(1) as f32;
    let c: Vec<f32> = x.into_iter().map(|v| v - mean).collect();
    let e = c.iter().map(|v| v * v).sum();
    (c, e)
}

/// The delay time in ms and its excess correlation, or `None` when no lag
/// clears [`ECHO_THRESHOLD`]. `target` and `dry` are aligned, at `rate`.
pub fn echo_delay(target: &[f32], dry: &[f32], rate: u32) -> Option<(f32, f32)> {
    let (ms, score) = echo_score(target, dry, rate);
    (score >= ECHO_THRESHOLD).then_some((ms, score))
}

/// The most echo-like lag (ms) and its excess correlation, with no
/// threshold (0 when the signals are too short).
pub fn echo_score(target: &[f32], dry: &[f32], rate: u32) -> (f32, f32) {
    let (t, te) = centred(onset_envelope(target));
    let (d, de) = centred(onset_envelope(dry));
    let hop_secs = HOP as f64 / rate as f64;
    let lo = (MIN_DELAY / hop_secs) as usize;
    let hi = ((MAX_DELAY / hop_secs) as usize).min(t.len() / 2);
    if hi <= lo + 2 {
        return (0.0, 0.0);
    }
    let excess: Vec<f32> = (lo..=hi)
        .map(|lag| autocorr(&t, lag, te) - autocorr(&d, lag, de).max(0.0))
        .collect();
    // Prefer the shortest strong lag: a feedback delay also correlates at
    // twice and three times its time.
    let best = excess.iter().copied().fold(f32::MIN, f32::max);
    let Some(first) = excess.iter().position(|&v| v >= 0.7 * best) else {
        return (0.0, 0.0);
    };
    // Refine to the local maximum around the first strong lag.
    let mut k = first;
    while k + 1 < excess.len() && excess[k + 1] > excess[k] {
        k += 1;
    }
    let ms = ((lo + k) as f64 * hop_secs * 1000.0) as f32;
    (ms, excess[k])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plucks at irregular times (no periodicity of their own).
    fn plucks(rate: u32, secs: f32) -> Vec<f32> {
        let mut x = vec![0f32; (secs * rate as f32) as usize];
        let times = [0.3, 0.75, 1.4, 1.65, 2.5, 3.1, 3.35, 4.2, 4.9, 5.3, 6.1, 6.45, 7.3];
        for (k, &t) in times.iter().enumerate() {
            let start = (t * rate as f32) as usize;
            let f = 110.0 * (1.0 + k as f32 * 0.17);
            for i in 0..(0.25 * rate as f32) as usize {
                if start + i < x.len() {
                    let tt = i as f32 / rate as f32;
                    x[start + i] += (std::f32::consts::TAU * f * tt).sin() * (-tt * 12.0).exp();
                }
            }
        }
        x
    }

    #[test]
    fn finds_the_echo_time_and_nothing_without_echo() {
        let rate = 48_000;
        let dry = plucks(rate, 8.0);
        let lag = (0.35 * rate as f32) as usize;
        let mut wet = dry.clone();
        for i in lag..wet.len() {
            wet[i] += 0.5 * dry[i - lag];
        }
        let (ms, _) = echo_delay(&wet, &dry, rate).expect("an echo");
        assert!((ms - 350.0).abs() < 15.0, "{ms} ms");
        assert!(echo_delay(&dry, &dry, rate).is_none());
    }
}
