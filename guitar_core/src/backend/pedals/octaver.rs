use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::OctaverParams},
};

/// Classic analog-style octaver: full-wave rectification of the input
/// doubles the apparent frequency content of the signal envelope, which
/// (after removing its DC offset and smoothing with a low-pass filter)
/// approximates a buzzy one-octave-down tone without true pitch tracking.
pub struct Octaver {
    params: OctaverParams,
    // Running DC average of the rectified signal, used to AC-couple it.
    dc_average: f32,
    // State for the one-pole low-pass tone filter.
    filtered: f32,
}

impl Octaver {
    pub fn new(params: OctaverParams) -> Self {
        Self {
            params,
            dc_average: 0.0,
            filtered: 0.0,
        }
    }

    /// One-pole low-pass filter, cutoff controlled by `tone` (0..1).
    fn apply_tone_filter(&mut self, input: f32) -> f32 {
        let cutoff = 20.0 + 4980.0 * self.params.tone.powf(2.0);
        let omega = 2.0 * std::f32::consts::PI * cutoff / GLOBAL_CONFIG.sample_rate;
        let alpha = omega / (1.0 + omega);

        self.filtered = alpha * input + (1.0 - alpha) * self.filtered;
        self.filtered
    }

    /// Full-wave rectify, remove DC offset via a slow running average, then
    /// low-pass filter to smooth the buzzy rectified waveform.
    fn octave_down(&mut self, input: f32) -> f32 {
        let rectified = input.abs();

        // Slow-moving DC average to AC-couple the rectified signal.
        const DC_ALPHA: f32 = 0.001;
        self.dc_average = DC_ALPHA * rectified + (1.0 - DC_ALPHA) * self.dc_average;
        let centered = rectified - self.dc_average;

        self.apply_tone_filter(centered)
    }
}

impl Pedal for Octaver {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32 / 32768.0;

        let octave_signal = self.octave_down(input_f);

        let output = self.params.dry_mix * input_f + self.params.octave_down_mix * octave_signal;

        (output.clamp(-1.0, 1.0) * 32768.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_dry_passes_through_input() {
        let params = OctaverParams {
            octave_down_mix: 0.0,
            dry_mix: 1.0,
            tone: 0.5,
        };
        let mut octaver = Octaver::new(params);

        for i in 0..200 {
            let input = ((i as f32 * 0.1).sin() * 10000.0) as i16;
            let output = octaver.apply_effect(input);
            assert_eq!(output, input);
        }
    }

    #[test]
    fn octave_mix_changes_output_from_dry_passthrough() {
        let dry_params = OctaverParams {
            octave_down_mix: 0.0,
            dry_mix: 1.0,
            tone: 0.5,
        };
        let wet_params = OctaverParams {
            octave_down_mix: 0.8,
            dry_mix: 0.5,
            tone: 0.5,
        };

        let mut dry_octaver = Octaver::new(dry_params);
        let mut wet_octaver = Octaver::new(wet_params);

        let mut any_different = false;
        for i in 0..1000 {
            // Periodic input signal to excite the rectification-based effect.
            let input = ((i as f32 * 0.05).sin() * 15000.0) as i16;
            let dry_output = dry_octaver.apply_effect(input);
            let wet_output = wet_octaver.apply_effect(input);
            if dry_output != wet_output {
                any_different = true;
            }
        }

        assert!(
            any_different,
            "expected octave-down mix to audibly differ from pure dry passthrough"
        );
    }
}
