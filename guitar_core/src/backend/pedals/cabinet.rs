use std::collections::VecDeque;

use crate::{
    backend::{dsp::convolution::PartitionedConvolver, pedals::pedal::Pedal},
    shared::pedals::CabinetParams,
};

/// FFT partition size used by the `PartitionedConvolver`. Bounds this pedal's
/// self-inflicted latency to one block (~21ms at 48kHz) regardless of IR length.
const BLOCK_SIZE: usize = 1024;

/// Loads a `.wav` file as mono `f32` samples in `[-1.0, 1.0]`, downmixing by
/// averaging channels if the file isn't already mono. No resampling is done:
/// IRs recorded at a rate other than `GLOBAL_CONFIG`'s 48kHz will play back
/// pitch-shifted, matching this project's existing non-negotiation of sample
/// rate elsewhere (see `capture.rs`).
fn load_ir_mono(path: &str) -> Option<Vec<f32>> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max)
                .collect()
        }
    };

    if channels <= 1 {
        return Some(samples);
    }
    Some(
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect(),
    )
}

/// A cabinet/speaker impulse-response simulator. Loads a `.wav` IR (named by
/// `CabinetParams::ir_path`) and convolves the live signal against it via a
/// `PartitionedConvolver`.
///
/// `Pedal::apply_effect` is sample-at-a-time, but FFT convolution is inherently
/// block-based, so incoming samples are buffered into `input_buffer` until a
/// full `BLOCK_SIZE` block accumulates; that block is then convolved in one
/// shot and its output samples are drained one at a time from `output_queue`
/// on subsequent calls. This adds up to `BLOCK_SIZE` samples (~21ms at 48kHz)
/// of latency local to this pedal -- acceptable for a cabinet sim (not a
/// time-critical modulation effect), but worth flagging given this project's
/// overall latency-consciousness (see `capture.rs`'s own doc comments).
pub struct Cabinet {
    convolver: Option<PartitionedConvolver>,
    mix: f32,
    input_buffer: Vec<f32>,
    output_queue: VecDeque<i16>,
}

impl Cabinet {
    pub fn new(params: CabinetParams) -> Self {
        let convolver = params
            .ir_path
            .as_deref()
            .and_then(load_ir_mono)
            .map(|ir| PartitionedConvolver::new(&ir, BLOCK_SIZE));

        Self {
            convolver,
            mix: params.mix,
            input_buffer: Vec::with_capacity(BLOCK_SIZE),
            output_queue: VecDeque::with_capacity(BLOCK_SIZE),
        }
    }
}

impl Pedal for Cabinet {
    fn apply_effect(&mut self, input: i16) -> i16 {
        // No IR loaded: pass the signal through unchanged rather than silencing it.
        let Some(convolver) = &mut self.convolver else {
            return input;
        };

        self.input_buffer.push(input as f32 / 32768.0);

        if self.input_buffer.len() == BLOCK_SIZE {
            let wet_block = convolver.process_block(&self.input_buffer);
            for (&dry, &wet) in self.input_buffer.iter().zip(wet_block.iter()) {
                let mixed = dry * (1.0 - self.mix) + wet * self.mix;
                self.output_queue
                    .push_back((mixed * 32768.0).clamp(-32768.0, 32767.0) as i16);
            }
            self.input_buffer.clear();
        }

        // While the first block is still filling, there's nothing to emit yet.
        self.output_queue.pop_front().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypasses_input_unchanged_when_no_ir_loaded() {
        let mut cabinet = Cabinet::new(CabinetParams {
            ir_path: None,
            mix: 1.0,
        });

        for sample in [0i16, 1000, -1000, 32000, -32000] {
            assert_eq!(cabinet.apply_effect(sample), sample);
        }
    }

    #[test]
    fn missing_ir_file_falls_back_to_bypass() {
        let mut cabinet = Cabinet::new(CabinetParams {
            ir_path: Some("/nonexistent/path/to/ir.wav".to_string()),
            mix: 1.0,
        });

        assert_eq!(cabinet.apply_effect(1234), 1234);
    }
}
