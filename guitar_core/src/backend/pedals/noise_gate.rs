use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::NoiseGateParams},
};

/// Standard noise gate: tracks the envelope of the (rectified) input signal and
/// smoothly attenuates it to (near) zero whenever the envelope drops below
/// `threshold`. Uses an attack/hold/release state machine so the gate opens
/// quickly on transients, stays open for `hold_ms` after the signal dips below
/// threshold (to avoid audible chatter on decaying notes), and then closes over
/// `release_ms`.
pub struct NoiseGate {
    params: NoiseGateParams,
    // Current gain multiplier applied to the signal (0.0 = fully closed, 1.0 = fully open).
    gain: f32,
    // Per-sample coefficients derived from the ms-based params.
    attack_coeff: f32,
    release_coeff: f32,
    hold_samples: u32,
    // Number of samples remaining in the hold window before release starts.
    hold_counter: u32,
}

impl NoiseGate {
    pub fn new(params: NoiseGateParams) -> Self {
        let attack_coeff = Self::rate_coeff(params.attack_ms);
        let release_coeff = Self::rate_coeff(params.release_ms);
        let hold_samples = ((params.hold_ms / 1000.0) * GLOBAL_CONFIG.sample_rate).max(0.0) as u32;

        Self {
            params,
            gain: 0.0,
            attack_coeff,
            release_coeff,
            hold_samples,
            hold_counter: 0,
        }
    }

    /// Converts a time constant in milliseconds into a per-sample one-pole
    /// smoothing coefficient (how much of the gap to the target is closed each
    /// sample). Guards against a zero/near-zero ms value, which would otherwise
    /// produce an instant (zipper-noise-prone) jump.
    fn rate_coeff(time_ms: f32) -> f32 {
        let time_ms = time_ms.max(0.01);
        let samples = (time_ms / 1000.0) * GLOBAL_CONFIG.sample_rate;
        1.0 - (-1.0 / samples.max(1.0)).exp()
    }
}

impl Pedal for NoiseGate {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32 / 32768.0;
        let level = input_f.abs();

        let target = if level >= self.params.threshold {
            self.hold_counter = self.hold_samples;
            1.0
        } else if self.hold_counter > 0 {
            self.hold_counter -= 1;
            1.0
        } else {
            0.0
        };

        let coeff = if target > self.gain {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.gain += (target - self.gain) * coeff;

        let output = input_f * self.gain;
        (output.clamp(-1.0, 1.0) * 32768.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_silences_signal_below_threshold() {
        let params = NoiseGateParams {
            threshold: 0.1,
            attack_ms: 1.0,
            release_ms: 5.0,
            hold_ms: 5.0,
        };
        let mut gate = NoiseGate::new(params);

        // Quiet noise well below threshold, fed long enough for the release +
        // hold window to fully elapse.
        let mut last_output = 0;
        for _ in 0..10_000 {
            last_output = gate.apply_effect(500); // ~0.015 amplitude, below 0.1 threshold
        }

        assert!(
            last_output.unsigned_abs() < 50,
            "expected near-silence once gate closes, got {last_output}"
        );
    }

    #[test]
    fn gate_passes_signal_above_threshold() {
        let params = NoiseGateParams {
            threshold: 0.1,
            attack_ms: 1.0,
            release_ms: 5.0,
            hold_ms: 5.0,
        };
        let mut gate = NoiseGate::new(params);

        // Loud signal, well above threshold. After enough samples for the
        // attack ramp to finish, output should be close to input.
        let input = 20000;
        let mut last_output = 0;
        for _ in 0..2000 {
            last_output = gate.apply_effect(input);
        }

        let ratio = last_output as f32 / input as f32;
        assert!(
            ratio > 0.95,
            "expected gate to be fully open and pass signal through, ratio was {ratio}"
        );
    }
}
