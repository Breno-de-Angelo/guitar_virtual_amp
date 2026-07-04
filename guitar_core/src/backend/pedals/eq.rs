use std::f32::consts::PI;

use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::EqParams},
};

const LOW_SHELF_FREQ: f32 = 100.0;
const MID_PEAK_FREQ: f32 = 1000.0;
const MID_PEAK_Q: f32 = 0.7;
const HIGH_SHELF_FREQ: f32 = 5000.0;

/// A single biquad (second-order IIR) filter section, using the standard
/// RBJ Audio EQ Cookbook Direct Form I difference equation.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// RBJ cookbook low shelf.
    fn low_shelf(frequency: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * frequency / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        // Shelf slope S = 1.0 (standard shelf slope), which simplifies the
        // RBJ cookbook's alpha term to sin(w0)/2 * sqrt(2).
        let alpha = sin_w0 / 2.0 * std::f32::consts::SQRT_2;
        let two_sqrt_a_alpha = 2.0 * f32::sqrt(a) * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Self::new(b0, b1, b2, a0, a1, a2)
    }

    /// RBJ cookbook high shelf.
    fn high_shelf(frequency: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * frequency / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0 * std::f32::consts::SQRT_2;
        let two_sqrt_a_alpha = 2.0 * f32::sqrt(a) * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Self::new(b0, b1, b2, a0, a1, a2)
    }

    /// RBJ cookbook peaking (bell) EQ.
    fn peaking(frequency: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * frequency / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        Self::new(b0, b1, b2, a0, a1, a2)
    }

    fn process(&mut self, x0: f32) -> f32 {
        let y0 = self.b0 * x0 + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = x0;
        self.y2 = self.y1;
        self.y1 = y0;

        y0
    }
}

/// A simple 3-band EQ pedal (low shelf, mid peak, high shelf) built from a
/// series bank of biquad filters. Named `EqPedal` rather than `Eq` to avoid
/// shadowing/confusion with `std::cmp::Eq`.
pub struct EqPedal {
    low_shelf: Biquad,
    mid_peak: Biquad,
    high_shelf: Biquad,
}

impl EqPedal {
    pub fn new(params: EqParams) -> Self {
        let sample_rate = GLOBAL_CONFIG.sample_rate;
        Self {
            low_shelf: Biquad::low_shelf(LOW_SHELF_FREQ, params.low_gain_db, sample_rate),
            mid_peak: Biquad::peaking(MID_PEAK_FREQ, MID_PEAK_Q, params.mid_gain_db, sample_rate),
            high_shelf: Biquad::high_shelf(HIGH_SHELF_FREQ, params.high_gain_db, sample_rate),
        }
    }
}

impl Pedal for EqPedal {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let sample = input as f32;
        let sample = self.low_shelf.process(sample);
        let sample = self.mid_peak.process(sample);
        let sample = self.high_shelf.process(sample);
        sample.clamp(-32768.0, 32767.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(frequency: f32, sample_rate: f32, num_samples: usize, amplitude: f32) -> Vec<i16> {
        (0..num_samples)
            .map(|n| {
                let t = n as f32 / sample_rate;
                (amplitude * (2.0 * PI * frequency * t).sin()) as i16
            })
            .collect()
    }

    fn rms(samples: &[i16]) -> f64 {
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / samples.len() as f64).sqrt()
    }

    #[test]
    fn flat_eq_leaves_amplitude_roughly_unchanged() {
        let sample_rate = GLOBAL_CONFIG.sample_rate;
        let input = sine_wave(440.0, sample_rate, 4096, 10000.0);

        let mut eq = EqPedal::new(EqParams::default());
        let output: Vec<i16> = input.iter().map(|&s| eq.apply_effect(s)).collect();

        // Skip the initial transient while the filters settle.
        let input_rms = rms(&input[1024..]);
        let output_rms = rms(&output[1024..]);

        let ratio = output_rms / input_rms;
        assert!(
            (0.9..=1.1).contains(&ratio),
            "expected flat EQ to roughly preserve amplitude, got ratio {}",
            ratio
        );
    }

    #[test]
    fn boosted_low_band_increases_low_frequency_amplitude() {
        let sample_rate = GLOBAL_CONFIG.sample_rate;
        let input = sine_wave(80.0, sample_rate, 4096, 10000.0);

        let mut flat_eq = EqPedal::new(EqParams::default());
        let flat_output: Vec<i16> = input.iter().map(|&s| flat_eq.apply_effect(s)).collect();

        let mut boosted_eq = EqPedal::new(EqParams {
            low_gain_db: 12.0,
            mid_gain_db: 0.0,
            high_gain_db: 0.0,
        });
        let boosted_output: Vec<i16> = input.iter().map(|&s| boosted_eq.apply_effect(s)).collect();

        let flat_rms = rms(&flat_output[1024..]);
        let boosted_rms = rms(&boosted_output[1024..]);

        assert!(
            boosted_rms > flat_rms,
            "expected boosted low band to increase RMS amplitude: flat={}, boosted={}",
            flat_rms,
            boosted_rms
        );
    }
}
