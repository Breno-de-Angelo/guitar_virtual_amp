//! The final linear match: the EQ the search measured (dB per band, see
//! [`super::features::Comparison`]) turned into a minimum-phase impulse
//! response that the Cabinet pedal can load, the way Tone Match products
//! export their filter as a cabinet IR.

use rustfft::{FftPlanner, num_complex::Complex32};
use std::path::Path;

/// IR length: 2048 samples (43 ms at 48 kHz), enough for 1/3-octave detail
/// down to ~60 Hz.
pub const IR_LEN: usize = 2048;

/// The EQ in dB at `freq`, interpolated on a log frequency axis (flat
/// beyond the outer bands).
fn eq_at(freq: f32, eq_db: &[f32], eq_hz: &[f32]) -> f32 {
    if eq_hz.is_empty() {
        return 0.0;
    }
    if freq <= eq_hz[0] {
        return eq_db[0];
    }
    for i in 1..eq_hz.len() {
        if freq <= eq_hz[i] {
            let t = (freq / eq_hz[i - 1]).ln() / (eq_hz[i] / eq_hz[i - 1]).ln();
            return eq_db[i - 1] + t * (eq_db[i] - eq_db[i - 1]);
        }
    }
    *eq_db.last().unwrap()
}

/// Minimum-phase IR with the magnitude response `eq_db` at `eq_hz`
/// (homomorphic method: real cepstrum of the log magnitude, folded).
pub fn ir_from_eq(eq_db: &[f32], eq_hz: &[f32], rate: u32) -> Vec<f32> {
    let n = IR_LEN * 4;
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    // Log magnitude (natural log of amplitude), symmetric spectrum.
    let mut buf: Vec<Complex32> = (0..n)
        .map(|k| {
            let k = if k <= n / 2 { k } else { n - k };
            let f = k as f32 * rate as f32 / n as f32;
            let db = eq_at(f.max(1.0), eq_db, eq_hz);
            Complex32::new(db / 20.0 * std::f32::consts::LN_10, 0.0)
        })
        .collect();
    inv.process(&mut buf);
    let scale = 1.0 / n as f32;
    // Fold the cepstrum: keep c0 and c[n/2], double the causal part.
    for (k, c) in buf.iter_mut().enumerate() {
        let v = c.re * scale;
        *c = Complex32::new(
            if k == 0 || k == n / 2 {
                v
            } else if k < n / 2 {
                2.0 * v
            } else {
                0.0
            },
            0.0,
        );
    }
    fwd.process(&mut buf);
    for c in buf.iter_mut() {
        *c = c.exp();
    }
    inv.process(&mut buf);
    let mut ir: Vec<f32> = buf[..IR_LEN].iter().map(|c| c.re * scale).collect();
    // Fade the last quarter so the truncation doesn't ring.
    let fade = IR_LEN / 4;
    for i in 0..fade {
        let w = 0.5 + 0.5 * (std::f32::consts::PI * i as f32 / fade as f32).cos();
        ir[IR_LEN - fade + i] *= w;
    }
    ir
}

/// `x` convolved with `ir`, same length as `x` (FFT convolution).
pub fn convolve(x: &[f32], ir: &[f32]) -> Vec<f32> {
    let n = (x.len() + ir.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut a: Vec<Complex32> = (0..n)
        .map(|i| Complex32::new(x.get(i).copied().unwrap_or(0.0), 0.0))
        .collect();
    let mut b: Vec<Complex32> = (0..n)
        .map(|i| Complex32::new(ir.get(i).copied().unwrap_or(0.0), 0.0))
        .collect();
    fwd.process(&mut a);
    fwd.process(&mut b);
    for (u, v) in a.iter_mut().zip(&b) {
        *u *= v;
    }
    inv.process(&mut a);
    a[..x.len()].iter().map(|c| c.re / n as f32).collect()
}

/// Writes an IR as a mono 32-bit float WAV that the Cabinet pedal loads.
pub fn write_ir(path: &Path, ir: &[f32], rate: u32) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &s in ir {
        w.write_sample(s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude in dB of `ir` at `freq`.
    fn response_db(ir: &[f32], freq: f32, rate: u32) -> f32 {
        let w = std::f32::consts::TAU * freq / rate as f32;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, h) in ir.iter().enumerate() {
            re += h * (w * i as f32).cos();
            im -= h * (w * i as f32).sin();
        }
        20.0 * (re * re + im * im).sqrt().log10()
    }

    #[test]
    fn the_ir_has_the_requested_response_and_is_front_loaded() {
        let eq_hz = [100.0, 400.0, 1600.0, 6400.0];
        let eq_db = [6.0, 0.0, -3.0, -12.0];
        let ir = ir_from_eq(&eq_db, &eq_hz, 48_000);
        for (f, d) in eq_hz.iter().zip(eq_db) {
            let got = response_db(&ir, *f, 48_000);
            assert!((got - d).abs() < 1.0, "{f} Hz: {got} dB, wanted {d}");
        }
        // Minimum phase: most of the energy at the start.
        let e: f32 = ir.iter().map(|v| v * v).sum();
        let early: f32 = ir[..64].iter().map(|v| v * v).sum();
        assert!(early > 0.8 * e);
    }

    #[test]
    fn convolving_with_a_unit_impulse_is_identity() {
        let x: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin()).collect();
        let y = convolve(&x, &[1.0]);
        assert!(x.iter().zip(&y).all(|(a, b)| (a - b).abs() < 1e-4));
    }
}
