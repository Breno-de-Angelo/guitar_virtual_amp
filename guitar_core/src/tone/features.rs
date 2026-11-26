//! Spectral features for comparing a rendered tone with a target.
//!
//! Both signals are loudness-normalised, then turned into log band energies
//! (dB) at two STFT resolutions. The search distance is "up to a smooth EQ":
//! before comparing frames, the render gets the smoothed, capped difference
//! of the two long-term spectra added to it. That lets the search fit the
//! nonlinear and time-varying part of the chain (gain, compression, drive,
//! reverb) while the linear colour of cabinet, pickups and mix is left to
//! the final matching filter ([`super::matching`]), as Tone Match products
//! do after the gain is right. The benchmark uses [`plain_distance`], with
//! no EQ allowance.

use rustfft::{FftPlanner, num_complex::Complex32};

/// Analysis resolutions: (FFT size, hop, lowest band edge in Hz, bands).
const RESOLUTIONS: [(usize, usize, f32, usize); 2] = [(2048, 512, 60.0, 40), (512, 128, 250.0, 20)];
const HIGHEST_HZ: f32 = 16_000.0;
/// Band energies below the loudest frame's level minus this are floored.
const FLOOR_DB: f32 = 80.0;
/// Frames quieter than the loudest minus this don't count (silence).
const ACTIVE_DB: f32 = 45.0;
/// Largest boost or cut the matching EQ may apply.
pub const MAX_EQ_DB: f32 = 18.0;

/// Triangular bands on a log frequency axis for one FFT size.
struct Bands {
    /// Per band: (bin, weight).
    weights: Vec<Vec<(usize, f32)>>,
    centers: Vec<f32>,
}

impl Bands {
    fn new(fft: usize, rate: u32, low: f32, count: usize) -> Self {
        let bin_hz = rate as f32 / fft as f32;
        let edges: Vec<f32> = (0..count + 2)
            .map(|i| low * (HIGHEST_HZ / low).powf(i as f32 / (count + 1) as f32))
            .collect();
        let mut weights = Vec::with_capacity(count);
        let mut centers = Vec::with_capacity(count);
        for b in 0..count {
            let (lo, mid, hi) = (edges[b], edges[b + 1], edges[b + 2]);
            let mut w: Vec<(usize, f32)> = (1..fft / 2)
                .filter_map(|k| {
                    let f = k as f32 * bin_hz;
                    let v = if f <= lo || f >= hi {
                        0.0
                    } else if f <= mid {
                        (f - lo) / (mid - lo)
                    } else {
                        (hi - f) / (hi - mid)
                    };
                    (v > 0.0).then_some((k, v))
                })
                .collect();
            if w.is_empty() {
                w.push((((mid / bin_hz).round() as usize).clamp(1, fft / 2 - 1), 1.0));
            }
            weights.push(w);
            centers.push(mid);
        }
        Self { weights, centers }
    }
}

/// Log band energies, frames x bands, row-major.
#[derive(Clone, Debug)]
pub struct Spectrogram {
    pub frames: usize,
    pub bands: usize,
    pub db: Vec<f32>,
}

impl Spectrogram {
    fn frame(&self, t: usize) -> &[f32] {
        &self.db[t * self.bands..(t + 1) * self.bands]
    }
}

/// Scales `x` to RMS 1 (silence stays silent).
pub fn normalized(x: &[f32]) -> Vec<f32> {
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    if rms < 1e-9 {
        return x.to_vec();
    }
    x.iter().map(|v| v / rms).collect()
}

fn spectrogram(x: &[f32], fft: usize, hop: usize, bands: &Bands) -> Spectrogram {
    let window: Vec<f32> = (0..fft)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / fft as f32).cos())
        .collect();
    let plan = FftPlanner::<f32>::new().plan_fft_forward(fft);
    let frames = if x.len() >= fft {
        (x.len() - fft) / hop + 1
    } else {
        0
    };
    let mut db = Vec::with_capacity(frames * bands.weights.len());
    let mut buf = vec![Complex32::new(0.0, 0.0); fft];
    let mut power = vec![0f32; fft / 2];
    for t in 0..frames {
        let start = t * hop;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex32::new(x[start + i] * window[i], 0.0);
        }
        plan.process(&mut buf);
        for (p, b) in power.iter_mut().zip(&buf) {
            *p = b.norm_sqr();
        }
        for w in &bands.weights {
            let e: f32 = w.iter().map(|&(k, v)| power[k] * v).sum();
            db.push(10.0 * (e + 1e-12).log10());
        }
    }
    let bands_n = bands.weights.len();
    let peak = db.iter().copied().fold(f32::MIN, f32::max);
    for v in &mut db {
        *v = v.max(peak - FLOOR_DB);
    }
    Spectrogram {
        frames,
        bands: bands_n,
        db,
    }
}

/// Frame loudness (dB of the summed band energies).
fn frame_level(s: &Spectrogram, t: usize) -> f32 {
    let e: f32 = s.frame(t).iter().map(|v| 10f32.powf(v / 10.0)).sum();
    10.0 * (e + 1e-12).log10()
}

/// Mean log spectrum over the given frames.
fn long_term(s: &Spectrogram, frames: &[usize]) -> Vec<f32> {
    let mut m = vec![0f32; s.bands];
    for &t in frames {
        for (acc, v) in m.iter_mut().zip(s.frame(t)) {
            *acc += v;
        }
    }
    let n = frames.len().max(1) as f32;
    m.iter_mut().for_each(|v| *v /= n);
    m
}

/// `diff` centred (mean 0 dB) and capped. The overlapping triangular bands
/// already make it smooth in frequency.
fn centred_eq(diff: &[f32]) -> Vec<f32> {
    let mean = diff.iter().sum::<f32>() / diff.len().max(1) as f32;
    diff.iter()
        .map(|v| (v - mean).clamp(-MAX_EQ_DB, MAX_EQ_DB))
        .collect()
}

struct Resolution {
    fft: usize,
    hop: usize,
    bands: Bands,
    spec: Spectrogram,
    active: Vec<usize>,
    long_term: Vec<f32>,
}

/// A target signal, analysed once and compared with many renders.
pub struct Target {
    rate: u32,
    len: usize,
    resolutions: Vec<Resolution>,
}

/// One comparison with a [`Target`].
#[derive(Clone, Debug)]
pub struct Comparison {
    /// Mean absolute dB difference after the EQ allowance, averaged over
    /// resolutions.
    pub distance: f32,
    /// The EQ (dB per band of the finest-frequency resolution) that the
    /// render needs to match the target's long-term spectrum.
    pub eq_db: Vec<f32>,
    /// Centre frequencies of those bands.
    pub eq_hz: Vec<f32>,
}

impl Target {
    pub fn new(x: &[f32], rate: u32) -> Self {
        let x = normalized(x);
        let resolutions = RESOLUTIONS
            .iter()
            .map(|&(fft, hop, low, count)| {
                let bands = Bands::new(fft, rate, low, count);
                let spec = spectrogram(&x, fft, hop, &bands);
                let levels: Vec<f32> = (0..spec.frames).map(|t| frame_level(&spec, t)).collect();
                let loudest = levels.iter().copied().fold(f32::MIN, f32::max);
                let active: Vec<usize> = (0..spec.frames)
                    .filter(|&t| levels[t] > loudest - ACTIVE_DB)
                    .collect();
                let long_term = long_term(&spec, &active);
                Resolution {
                    fft,
                    hop,
                    bands,
                    spec,
                    active,
                    long_term,
                }
            })
            .collect();
        Self {
            rate,
            len: x.len(),
            resolutions,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Compares a render (same length and alignment as the target).
    pub fn compare(&self, render: &[f32]) -> Comparison {
        let render = normalized(render);
        let mut total = 0.0;
        let mut first_eq = None;
        for r in &self.resolutions {
            let spec = spectrogram(&render, r.fft, r.hop, &r.bands);
            let frames: Vec<usize> = r
                .active
                .iter()
                .copied()
                .filter(|&t| t < spec.frames)
                .collect();
            let rendered = long_term(&spec, &frames);
            let diff: Vec<f32> = r
                .long_term
                .iter()
                .zip(&rendered)
                .map(|(a, b)| a - b)
                .collect();
            let eq = centred_eq(&diff);
            // The EQ is centred; the level difference it leaves is loudness,
            // which the normalisation already took out, so add it back.
            let offset = diff.iter().sum::<f32>() / diff.len().max(1) as f32;
            let mut sum = 0.0;
            for &t in &frames {
                for ((a, b), e) in r.spec.frame(t).iter().zip(spec.frame(t)).zip(&eq) {
                    sum += (a - (b + e + offset)).abs();
                }
            }
            total += sum / (frames.len().max(1) * r.spec.bands) as f32;
            first_eq.get_or_insert((eq, r.bands.centers.clone()));
        }
        let (eq_db, eq_hz) = first_eq.unwrap_or_default();
        Comparison {
            distance: total / self.resolutions.len() as f32,
            eq_db,
            eq_hz,
        }
    }
}

/// Mean absolute log band difference between two loudness-normalised
/// signals over the target's active frames, with no EQ allowance: how far
/// apart they sound, for scoring.
pub fn plain_distance(target: &[f32], render: &[f32], rate: u32) -> f32 {
    let t = Target::new(target, rate);
    let render = normalized(render);
    let mut total = 0.0;
    for r in &t.resolutions {
        let spec = spectrogram(&render, r.fft, r.hop, &r.bands);
        let frames: Vec<usize> = r
            .active
            .iter()
            .copied()
            .filter(|&f| f < spec.frames)
            .collect();
        let mut sum = 0.0;
        for &f in &frames {
            for (a, b) in r.spec.frame(f).iter().zip(spec.frame(f)) {
                sum += (a - b).abs();
            }
        }
        total += sum / (frames.len().max(1) * r.spec.bands) as f32;
    }
    total / t.resolutions.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeated decaying notes with 20 harmonics.
    fn tone(freq: f32, secs: f32, rate: u32) -> Vec<f32> {
        (0..(secs * rate as f32) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let env = (t * 3.0).fract();
                let wave: f32 = (1..=20)
                    .map(|h| (std::f32::consts::TAU * freq * h as f32 * t).sin() / h as f32)
                    .sum();
                wave * (1.0 - env)
            })
            .collect()
    }

    #[test]
    fn identical_signals_are_at_distance_zero() {
        let x = tone(220.0, 2.0, 48_000);
        let t = Target::new(&x, 48_000);
        assert!(t.compare(&x).distance < 1e-3);
        assert!(plain_distance(&x, &x, 48_000) < 1e-3);
    }

    #[test]
    fn level_and_linear_colour_are_forgiven_but_not_by_plain_distance() {
        let x = tone(220.0, 2.0, 48_000);
        // A louder copy through a one-pole low-pass: a linear change.
        let mut y = Vec::with_capacity(x.len());
        let mut s = 0.0;
        for v in &x {
            s = 0.7 * s + 0.3 * v * 4.0;
            y.push(s);
        }
        let t = Target::new(&x, 48_000);
        let with_eq = t.compare(&y).distance;
        let plain = plain_distance(&x, &y, 48_000);
        assert!(with_eq < plain * 0.5, "{with_eq} vs {plain}");
    }

    #[test]
    fn distortion_is_not_forgiven() {
        let x = tone(110.0, 2.0, 48_000);
        let clipped: Vec<f32> = x.iter().map(|v| (v * 20.0).tanh()).collect();
        let t = Target::new(&x, 48_000);
        assert!(t.compare(&clipped).distance > 2.0);
    }
}
