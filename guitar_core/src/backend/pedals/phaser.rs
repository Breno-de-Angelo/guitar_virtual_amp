use crate::{
    backend::{dsp::all_pass::Allpass, pedals::pedal::Pedal},
    shared::{config::GLOBAL_CONFIG, pedals::PhaserParams},
};

const NUM_STAGES: usize = 4;
const MIN_DELAY_SAMPLES: f32 = 1.0;
const MAX_DELAY_SAMPLES: f32 = 40.0;

pub struct Phaser {
    params: PhaserParams,
    lfo_phase: f32,
    stages: Vec<Allpass>,
}

impl Phaser {
    pub fn new(params: PhaserParams) -> Self {
        let feedback = params.feedback.clamp(0.0, 0.95);
        let mut stages = Vec::with_capacity(NUM_STAGES);
        for _ in 0..NUM_STAGES {
            stages.push(Allpass::new(MIN_DELAY_SAMPLES as usize, feedback));
        }
        Self {
            params,
            lfo_phase: 0.0,
            stages,
        }
    }
}

impl Phaser {
    /// Runs the phaser's float-domain processing without clamping/casting to i16.
    /// Kept separate from `apply_effect` so tests can inspect the raw (unclamped)
    /// output for stability (NaN/inf) checks.
    fn process_f32(&mut self, input_f: f32) -> f32 {
        // Advance the LFO phase accumulator.
        self.lfo_phase += 2.0 * std::f32::consts::PI * self.params.rate_hz / GLOBAL_CONFIG.sample_rate;
        if self.lfo_phase >= 2.0 * std::f32::consts::PI {
            self.lfo_phase -= 2.0 * std::f32::consts::PI;
        }

        // Normalize the LFO to 0.0..=1.0 and scale by depth to get the sweep amount.
        let lfo_norm = (self.lfo_phase.sin() + 1.0) * 0.5;
        let depth = self.params.depth.clamp(0.0, 1.0);
        let sweep = lfo_norm * depth;
        let delay_len =
            (MIN_DELAY_SAMPLES + sweep * (MAX_DELAY_SAMPLES - MIN_DELAY_SAMPLES)).round() as usize;

        let feedback = self.params.feedback.clamp(0.0, 0.95);

        let mut wet = input_f;
        for stage in &mut self.stages {
            stage.set_delay_len(delay_len);
            stage.set_feedback(feedback);
            wet = stage.process(wet);
        }

        let mix = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - mix;
        input_f * dry + wet * mix
    }
}

impl Pedal for Phaser {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let output = self.process_f32(input as f32);
        output.clamp(-32768.0, 32767.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_varies_over_time_when_wet_and_depth_are_nonzero() {
        let params = PhaserParams {
            rate_hz: 2.0,
            depth: 1.0,
            feedback: 0.5,
            mix: 1.0,
        };
        let mut phaser = Phaser::new(params);

        // Feed a periodic (square-ish) input across several LFO cycles and collect the outputs.
        let sample_rate = GLOBAL_CONFIG.sample_rate;
        let num_samples = (sample_rate / params.rate_hz) as usize * 4; // ~4 LFO cycles
        let period = 50usize;
        let mut outputs = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let input = if (i / period) % 2 == 0 { 8000 } else { -8000 };
            outputs.push(phaser.apply_effect(input));
        }

        // The modulation should cause the output to take on more than a couple of distinct
        // values over time (i.e. it isn't just a static filter).
        let distinct: std::collections::HashSet<i16> = outputs.iter().copied().collect();
        assert!(
            distinct.len() > 4,
            "expected modulated output to vary over time, got {} distinct values",
            distinct.len()
        );
    }

    #[test]
    fn dry_passthrough_when_mix_is_zero() {
        let params = PhaserParams {
            rate_hz: 1.0,
            depth: 1.0,
            feedback: 0.8,
            mix: 0.0,
        };
        let mut phaser = Phaser::new(params);

        for i in 0..2000 {
            let input = ((i as f32 * 0.1).sin() * 10000.0) as i16;
            assert_eq!(phaser.apply_effect(input), input);
        }
    }

    #[test]
    fn stays_finite_with_feedback_near_max() {
        let params = PhaserParams {
            rate_hz: 5.0,
            depth: 1.0,
            feedback: 0.95,
            mix: 1.0,
        };
        let mut phaser = Phaser::new(params);

        for i in 0..20000 {
            let input = ((i as f32 * 0.05).sin() * 20000.0) as i16;
            let raw = phaser.process_f32(input as f32);
            assert!(raw.is_finite(), "phaser output became non-finite: {raw}");
        }
    }
}
