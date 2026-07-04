use crate::{
    backend::{dsp::biquad::Biquad, pedals::pedal::Pedal},
    shared::{config::GLOBAL_CONFIG, pedals::EqParams},
};

const LOW_SHELF_FREQ: f32 = 100.0;
const MID_PEAK_FREQ: f32 = 1000.0;
const MID_PEAK_Q: f32 = 0.7;
const HIGH_SHELF_FREQ: f32 = 5000.0;

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
    use std::f32::consts::PI;

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
