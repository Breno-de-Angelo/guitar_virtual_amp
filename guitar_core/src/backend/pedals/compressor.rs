use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::CompressorParams},
};

pub struct Compressor {
    params: CompressorParams,
    // Smoothed envelope estimate of |input| (in the 0.0..=1.0 normalized range)
    envelope: f32,
    // Per-sample smoothing coefficients derived from attack/release times
    attack_coeff: f32,
    release_coeff: f32,
}

impl Compressor {
    pub fn new(params: CompressorParams) -> Self {
        let attack_coeff = Self::time_to_coeff(params.attack_ms);
        let release_coeff = Self::time_to_coeff(params.release_ms);

        Self {
            params,
            envelope: 0.0,
            attack_coeff,
            release_coeff,
        }
    }

    /// Converts a time constant (ms) into a per-sample exponential smoothing
    /// coefficient at the global sample rate. Smaller times -> smaller
    /// coefficient -> the envelope reacts faster.
    fn time_to_coeff(time_ms: f32) -> f32 {
        let time_ms = time_ms.max(0.001);
        (-1.0 / (time_ms / 1000.0 * GLOBAL_CONFIG.sample_rate)).exp()
    }

    /// Updates the envelope follower with the current sample's absolute
    /// amplitude, using the attack coefficient while the signal is rising and
    /// the release coefficient while it's falling.
    fn update_envelope(&mut self, rectified: f32) {
        let coeff = if rectified > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope = coeff * self.envelope + (1.0 - coeff) * rectified;
    }

    /// Computes the gain factor to apply given the current envelope: 1.0
    /// (no reduction) below threshold, and a proportionally reduced gain
    /// above it based on `ratio`.
    fn gain_reduction(&self) -> f32 {
        if self.envelope <= self.params.threshold || self.params.ratio <= 1.0 {
            return 1.0;
        }

        // How far above threshold the envelope is, in dB-like log domain would
        // be more accurate, but a linear approximation keeps this simple and
        // matches the sample-at-a-time, no-alloc style used elsewhere.
        let excess = self.envelope - self.params.threshold;
        let compressed_excess = excess / self.params.ratio;
        let target_envelope = self.params.threshold + compressed_excess;

        target_envelope / self.envelope
    }
}

impl Pedal for Compressor {
    fn apply_effect(&mut self, input: i16) -> i16 {
        // Normalize input to [-1, 1]
        let input_f = input as f32 / 32768.0;

        // Track a smoothed envelope of the rectified signal
        self.update_envelope(input_f.abs());

        // Reduce gain above threshold, then restore level via makeup gain
        let gain = self.gain_reduction() * self.params.makeup_gain;
        let output = input_f * gain;

        (output.clamp(-1.0, 1.0) * 32768.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a sustained loud signal (well above threshold) through a
    /// compressing pedal (ratio = 4.0) and a non-compressing pedal
    /// (ratio = 1.0, same makeup gain) and confirms the compressed output's
    /// steady-state amplitude ends up lower than the uncompressed one -- i.e.
    /// gain reduction is actually happening for loud signals.
    #[test]
    fn compresses_loud_signal_more_than_ratio_one() {
        let base_params = CompressorParams {
            threshold: 0.3,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_gain: 1.0,
        };

        let mut compressed = Compressor::new(base_params);
        let mut uncompressed = Compressor::new(CompressorParams {
            ratio: 1.0,
            ..base_params
        });

        // Sustained square wave well above threshold (0.9 amplitude), enough
        // samples for the envelope follower to settle.
        let amplitude = 0.9_f32;
        let sample_pos = (amplitude * 32768.0) as i16;
        let sample_neg = -sample_pos;

        let mut last_compressed_abs = 0i32;
        let mut last_uncompressed_abs = 0i32;

        for i in 0..2000 {
            let input = if i % 2 == 0 { sample_pos } else { sample_neg };
            last_compressed_abs = compressed.apply_effect(input).unsigned_abs() as i32;
            last_uncompressed_abs = uncompressed.apply_effect(input).unsigned_abs() as i32;
        }

        assert!(
            last_compressed_abs < last_uncompressed_abs,
            "expected compressed output ({last_compressed_abs}) to be quieter than uncompressed ({last_uncompressed_abs})"
        );
    }
}
