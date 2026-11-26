use std::collections::VecDeque;
use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Hann-windowed magnitude spectrum in dBFS (a full-scale sine reads 0 dB),
/// with the FFT plan cached between calls.
pub struct SpectrumAnalyzer {
    size: usize,
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    scratch: Vec<Complex<f64>>,
}

impl SpectrumAnalyzer {
    /// `size` is rounded up to a power of two.
    pub fn new(size: usize) -> Self {
        let size = size.max(16).next_power_of_two();
        let fft = FftPlanner::new().plan_fft_forward(size);
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / size as f64).cos())
            .collect();
        Self { size, fft, window, scratch: vec![Complex::new(0.0, 0.0); size] }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// dB per bin `0..size/2` for the latest `size` samples (zero-padded at
    /// the start when there are fewer).
    pub fn db(&mut self, samples: &VecDeque<f64>) -> Vec<f64> {
        let n = self.size;
        let skip = samples.len().saturating_sub(n);
        let pad = n.saturating_sub(samples.len());
        for c in self.scratch.iter_mut().take(pad) {
            *c = Complex::new(0.0, 0.0);
        }
        for (i, &x) in samples.iter().skip(skip).enumerate() {
            self.scratch[pad + i] = Complex::new(x * self.window[pad + i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        // A Hann window's coherent gain is 0.5: a full-scale sine's peak bin
        // has magnitude n/4.
        let reference = n as f64 / 4.0;
        self.scratch[..n / 2].iter().map(|c| 20.0 * (c.norm() / reference).max(1e-9).log10()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_sine_peaks_near_zero_db_at_its_bin() {
        let rate = 48_000.0;
        let mut analyzer = SpectrumAnalyzer::new(4096);
        let bin = 200;
        let freq = bin as f64 * rate / 4096.0;
        let samples: VecDeque<f64> =
            (0..4096).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin()).collect();
        let db = analyzer.db(&samples);
        let (peak_bin, peak) = db.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        assert_eq!(peak_bin, bin);
        assert!(peak.abs() < 0.5, "{peak}");
        assert!(db[bin + 40] < -60.0);
    }

    #[test]
    fn short_input_is_zero_padded() {
        let mut analyzer = SpectrumAnalyzer::new(1000);
        assert_eq!(analyzer.size(), 1024);
        let db = analyzer.db(&VecDeque::from(vec![0.0; 10]));
        assert_eq!(db.len(), 512);
        assert!(db.iter().all(|d| *d <= -170.0));
    }
}
