use std::f32::consts::PI;

use crate::{
    backend::{pedals::pedal::Pedal, ring_buffer::RingBuffer},
    shared::{config::GLOBAL_CONFIG, pedals::ChorusParams},
};

pub struct Chorus {
    buffer: RingBuffer<i16>,
    lfo_phase: f32,
    omega: f32,
    params: ChorusParams,
}

impl Chorus {
    pub fn new(params: ChorusParams) -> Self {
        Self {
            buffer: RingBuffer::new(65536),
            lfo_phase: 0.0,
            omega: 2.0 * PI * params.rate_hz,
            params,
        }
    }
}

impl Pedal for Chorus {
    fn apply_effect(&mut self, input: i16) -> i16 {
        self.buffer.push(input);

        // Modulated delay time in samples, oscillating around delay_ms by +/- depth_ms.
        let delay_time_ms =
            self.params.delay_ms + self.params.depth_ms * self.lfo_phase.sin();
        let delay_samples =
            (delay_time_ms.max(0.0) * GLOBAL_CONFIG.sample_rate / 1000.0).max(0.0);

        let delay_floor = delay_samples.floor() as usize;
        let delay_frac = delay_samples - delay_floor as f32;

        let len = self.buffer.len();
        let delayed = if len > delay_floor + 1 {
            let a = self.buffer[len - 1 - delay_floor] as f32;
            let b = self.buffer[len - 2 - delay_floor] as f32;
            // Linear interpolation between adjacent samples for fractional delay.
            a + (b - a) * delay_frac
        } else if len > delay_floor {
            self.buffer[len - 1 - delay_floor] as f32
        } else {
            0.0
        };

        let wet = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;
        let output = (input as f32 * dry + delayed * wet).clamp(-32768.0, 32767.0) as i16;

        self.lfo_phase += self.omega / GLOBAL_CONFIG.sample_rate;
        if self.lfo_phase >= 2.0 * PI {
            self.lfo_phase -= 2.0 * PI;
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_mix_passes_input_through_unchanged() {
        let params = ChorusParams {
            delay_ms: 20.0,
            depth_ms: 5.0,
            rate_hz: 1.0,
            mix: 0.0,
        };
        let mut chorus = Chorus::new(params);

        for i in 0..2000 {
            let input = (1000.0 * (i as f32 * 0.05).sin()) as i16;
            assert_eq!(chorus.apply_effect(input), input);
        }
    }

    #[test]
    fn modulation_changes_output_relative_to_unmodulated_delay() {
        // Compare a chorus with LFO depth against one with depth = 0 (a plain
        // fixed delay) fed the same periodic signal. If the LFO modulation is
        // actually doing something, the two should diverge at some point.
        let modulated_params = ChorusParams {
            delay_ms: 20.0,
            depth_ms: 5.0,
            rate_hz: 1.0,
            mix: 1.0,
        };
        let fixed_params = ChorusParams {
            delay_ms: 20.0,
            depth_ms: 0.0,
            rate_hz: 1.0,
            mix: 1.0,
        };
        let mut modulated = Chorus::new(modulated_params);
        let mut fixed = Chorus::new(fixed_params);

        let n = GLOBAL_CONFIG.sample_rate as usize * 3;
        let mut diverged = false;
        for i in 0..n {
            let input = (5000.0 * (i as f32 * 0.05).sin()) as i16;
            let out_modulated = modulated.apply_effect(input);
            let out_fixed = fixed.apply_effect(input);
            if out_modulated != out_fixed {
                diverged = true;
            }
        }

        assert!(
            diverged,
            "expected LFO-modulated delay to diverge from a fixed delay at some point"
        );
    }
}
