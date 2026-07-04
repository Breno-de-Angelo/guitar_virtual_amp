use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::TremoloParams},
};

pub struct Tremolo {
    params: TremoloParams,
    lfo_phase: f32,
}

impl Tremolo {
    pub fn new(params: TremoloParams) -> Self {
        Self {
            params,
            lfo_phase: 0.0,
        }
    }
}

impl Pedal for Tremolo {
    fn apply_effect(&mut self, input: i16) -> i16 {
        // Update LFO phase
        self.lfo_phase += 2.0 * std::f32::consts::PI * self.params.rate_hz / GLOBAL_CONFIG.sample_rate;
        if self.lfo_phase >= 2.0 * std::f32::consts::PI {
            self.lfo_phase -= 2.0 * std::f32::consts::PI;
        }

        // Gain oscillates between 1.0 (LFO at peak) and 1.0 - depth (LFO at trough)
        let depth = self.params.depth.clamp(0.0, 1.0);
        let gain = 1.0 - depth * (0.5 - 0.5 * self.lfo_phase.sin());

        let output = input as f32 * gain;

        output.clamp(-32768.0, 32767.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tremolo_modulates_amplitude_without_boosting() {
        let params = TremoloParams {
            rate_hz: 5.0,
            depth: 0.8,
        };
        let mut tremolo = Tremolo::new(params);

        let input: i16 = 10000;
        // Cover more than one full LFO cycle: sample_rate / rate_hz samples make
        // one cycle, so use a few multiples of that.
        let samples_per_cycle = (GLOBAL_CONFIG.sample_rate / params.rate_hz).ceil() as usize;
        let total_samples = samples_per_cycle * 3;

        let outputs: Vec<i16> = (0..total_samples)
            .map(|_| tremolo.apply_effect(input))
            .collect();

        let min = *outputs.iter().min().unwrap();
        let max = *outputs.iter().max().unwrap();

        // The output amplitude must vary (not be constant) over a full LFO cycle.
        assert!(
            max != min,
            "expected tremolo output to vary, got constant {max}"
        );

        // Depth only attenuates -- output should never exceed the input amplitude.
        assert!(
            outputs.iter().all(|&s| s.abs() <= input),
            "tremolo output exceeded input amplitude"
        );
    }
}
