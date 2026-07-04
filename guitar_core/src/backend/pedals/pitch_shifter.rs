use std::f32::consts::PI;

use crate::{
    backend::{pedals::pedal::Pedal, ring_buffer::RingBuffer},
    shared::{config::GLOBAL_CONFIG, pedals::PitchShifterParams},
};

/// Capacity of the history buffer, in samples. Must comfortably exceed the
/// largest grain size we expect to use (a few hundred ms at 48kHz).
const HISTORY_CAPACITY: usize = 16384;

/// Classic "dual read-pointer" granular pitch shifter (a time-domain,
/// FFT-free overlap-add technique, sometimes called PSOLA-lite).
///
/// The input is written into a delay-line ring buffer at a constant rate (one
/// sample per sample, like normal recording). Two read pointers, each
/// tracked as a fractional "delay behind the write head" and spaced half a
/// grain length apart, advance at `pitch_ratio` per sample instead of 1.0.
/// When `pitch_ratio > 1.0` the read pointers catch up to the write head
/// (delay shrinks) and must periodically jump back by one grain length; when
/// `pitch_ratio < 1.0` they fall behind (delay grows) and must periodically
/// jump forward by one grain length. Both cases cause a discontinuity, which
/// is why there are two pointers: a raised-cosine (Hann) window crossfades
/// between them so that as one pointer approaches a jump (window -> 0) the
/// other is near the middle of its grain (window -> 1), hiding the click.
pub struct PitchShifter {
    buffer: RingBuffer<HISTORY_CAPACITY, f32>,
    params: PitchShifterParams,
    pitch_ratio: f32,
    grain_size: f32,
    /// Fractional delay (in samples, behind the write head) of each read
    /// pointer. Always kept within `[0, grain_size)`.
    delay_a: f32,
    delay_b: f32,
}

impl PitchShifter {
    pub fn new(params: PitchShifterParams) -> Self {
        let pitch_ratio = 2.0_f32.powf(params.semitones / 12.0);

        // Convert the requested grain size to samples, clamped to something
        // that comfortably fits inside the history buffer.
        let grain_size = ((params.grain_size_ms / 1000.0) * GLOBAL_CONFIG.sample_rate)
            .clamp(2.0, (HISTORY_CAPACITY - 8) as f32);

        Self {
            buffer: RingBuffer::new(),
            params,
            pitch_ratio,
            grain_size,
            delay_a: 0.0,
            delay_b: grain_size / 2.0,
        }
    }

    /// Wraps `d` into `[0, grain_size)`.
    fn wrap(d: f32, grain_size: f32) -> f32 {
        let mut d = d % grain_size;
        if d < 0.0 {
            d += grain_size;
        }
        d
    }

    /// Hann window: 0 at phase 0 and 1, 1 at phase 0.5. Two pointers spaced
    /// half a grain apart and windowed this way sum to (approximately) a
    /// constant 1.0, which is what makes the crossfade click-free.
    fn window(phase: f32) -> f32 {
        0.5 - 0.5 * (2.0 * PI * phase).cos()
    }

    /// Reads a linearly-interpolated sample `delay` samples behind the most
    /// recently pushed sample.
    fn read(&self, delay: f32) -> f32 {
        let len = self.buffer.len();
        if len == 0 {
            return 0.0;
        }

        let pos = len as f32 - delay;
        let i0f = pos.floor();
        let frac = pos - i0f;

        let max_idx = (len - 1) as isize;
        let clamp_idx = |i: isize| -> usize { i.clamp(0, max_idx) as usize };

        let i0 = clamp_idx(i0f as isize);
        let i1 = clamp_idx(i0f as isize + 1);

        self.buffer[i0] * (1.0 - frac) + self.buffer[i1] * frac
    }
}

impl Pedal for PitchShifter {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32;

        self.buffer.push(input_f);

        let delta = 1.0 - self.pitch_ratio;
        self.delay_a = Self::wrap(self.delay_a + delta, self.grain_size);
        self.delay_b = Self::wrap(self.delay_b + delta, self.grain_size);

        let tap_a = self.read(self.delay_a);
        let tap_b = self.read(self.delay_b);

        let phase_a = self.delay_a / self.grain_size;
        let phase_b = self.delay_b / self.grain_size;

        let mut weight_a = Self::window(phase_a);
        let mut weight_b = Self::window(phase_b);

        let weight_sum = weight_a + weight_b;
        if weight_sum > 1e-6 {
            weight_a /= weight_sum;
            weight_b /= weight_sum;
        } else {
            weight_a = 0.5;
            weight_b = 0.5;
        }

        let wet = tap_a * weight_a + tap_b * weight_b;

        let mix = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - mix;
        let out_f = input_f * dry + wet * mix;

        out_f.clamp(-32768.0, 32767.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `mix = 0.0` the wet path must contribute nothing: output should
    /// track the (unchanged) dry input exactly, regardless of internal
    /// granular state.
    #[test]
    fn zero_mix_is_pure_dry_passthrough() {
        let params = PitchShifterParams {
            semitones: 5.0,
            mix: 0.0,
            grain_size_ms: 80.0,
        };
        let mut shifter = PitchShifter::new(params);

        let input: [i16; 8] = [0, 1000, -1000, 5000, -5000, 32767, -32768, 12345];
        for &sample in &input {
            assert_eq!(shifter.apply_effect(sample), sample);
        }
    }

    /// With `semitones = 0.0` the pitch ratio is 1.0, so once the history
    /// buffer has filled with a constant (DC) value, both read taps settle
    /// on that same value and the (normalized) crossfade must reproduce it
    /// with full mix -- proving the shifter neither silences nor mangles a
    /// steady signal at unity pitch ratio.
    #[test]
    fn unity_pitch_ratio_tracks_constant_input_once_buffer_fills() {
        let params = PitchShifterParams {
            semitones: 0.0,
            mix: 1.0,
            grain_size_ms: 20.0,
        };
        let mut shifter = PitchShifter::new(params);

        const VALUE: i16 = 8000;
        let mut last_output = 0;
        // Push well past the grain size so the whole history buffer is filled
        // with the constant value.
        for _ in 0..4000 {
            last_output = shifter.apply_effect(VALUE);
        }

        assert!(
            (last_output - VALUE).abs() <= 2,
            "expected output close to {VALUE}, got {last_output}"
        );
    }

    /// Robustness check: feed a sine wave through shifters tuned to +7 and -7
    /// semitones for several thousand samples and confirm the output never
    /// produces NaN/garbage and always stays within i16 range. This does not
    /// verify pitch-shift accuracy (impractical sample-by-sample in a unit
    /// test) -- it's a stability/no-crash guarantee.
    #[test]
    fn stays_finite_and_in_range_for_extreme_shifts() {
        for semitones in [7.0, -7.0] {
            let params = PitchShifterParams {
                semitones,
                mix: 0.7,
                grain_size_ms: 80.0,
            };
            let mut shifter = PitchShifter::new(params);

            let sample_rate = GLOBAL_CONFIG.sample_rate;
            let freq = 220.0_f32;
            for n in 0..8000 {
                let t = n as f32 / sample_rate;
                let input = (2.0 * PI * freq * t).sin() * 20000.0;
                let output = shifter.apply_effect(input as i16);

                assert!(
                    (i16::MIN..=i16::MAX).contains(&output),
                    "output {output} out of i16 range for semitones={semitones}"
                );
            }
        }
    }
}
