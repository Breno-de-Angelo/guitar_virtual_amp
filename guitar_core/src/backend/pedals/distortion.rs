use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::DistortionParams},
};

pub struct Distortion {
    params: DistortionParams,
    // State variables for tone filter (one-pole low-pass)
    y1: f32,
}

impl Distortion {
    pub fn new(params: DistortionParams) -> Self {
        Self { params, y1: 0.0 }
    }

    /// Soft clipping using hyperbolic tangent for tube-like distortion
    fn soft_clip(&self, input: f32) -> f32 {
        (input * self.params.drive).tanh()
    }

    /// Hard clipping for more aggressive distortion
    fn hard_clip(&self, input: f32) -> f32 {
        (input * self.params.drive).clamp(-1.0, 1.0)
    }

    /// One-pole low-pass tone filter
    fn apply_tone_filter(&mut self, input: f32) -> f32 {
        // Map tone (0..1) to cutoff frequency in Hz using non-linear curve
        let cutoff = 20.0 + 19980.0 * self.params.tone.powf(2.0);
        let omega = 2.0 * std::f32::consts::PI * cutoff / GLOBAL_CONFIG.sample_rate;
        let alpha = omega / (1.0 + omega);

        let output = alpha * input + (1.0 - alpha) * self.y1;
        self.y1 = output;
        output
    }

    /// Apply distortion based on drive, bias, and soft/hard clipping
    fn apply_distortion(&self, input: f32) -> f32 {
        let biased = (input + self.params.bias).clamp(-1.0, 1.0);

        if self.params.drive < 2.0 {
            self.soft_clip(biased)
        } else if self.params.drive < 5.0 {
            let soft = self.soft_clip(biased);
            let hard = self.hard_clip(biased);
            let mix = (self.params.drive - 2.0) / 3.0;
            soft * (1.0 - mix) + hard * mix
        } else {
            self.hard_clip(biased)
        }
    }
}

impl Pedal for Distortion {
    fn apply_effect(&mut self, input: i16) -> i16 {
        // Normalize input to [-1,1]
        let input_f = input as f32 / 32768.0;

        // Apply distortion
        let distorted = self.apply_distortion(input_f);

        // Apply tone filter
        let filtered = self.apply_tone_filter(distorted);

        // Apply output level
        let leveled = filtered * self.params.level;

        // Mix wet/dry
        let wet = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;
        let output = input_f * dry + leveled * wet;

        // Clamp and convert back to i16
        (output.clamp(-1.0, 1.0) * 32768.0) as i16
    }
}
