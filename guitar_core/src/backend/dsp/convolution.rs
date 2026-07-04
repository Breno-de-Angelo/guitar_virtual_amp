use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Uniform-partitioned FFT convolution, implemented via the overlap-add method.
///
/// The impulse response is split into `block_size`-length partitions, each
/// zero-padded to `2 * block_size` and FFT'd once up front (in [`Self::new`]).
/// On every call to [`Self::process_block`] the incoming block is zero-padded
/// to `2 * block_size` and FFT'd, then pointwise-multiplied against every
/// partition's precomputed spectrum. Each of those products is inverse-FFT'd
/// back to the time domain, giving a `2 * block_size`-length linear
/// convolution result for that (input block, partition) pair, which is added
/// into a running accumulator at the time offset appropriate for that
/// partition's delay (partition `k` lands `k` blocks in the future). The
/// first `block_size` samples of the accumulator are emitted as output, and
/// the accumulator is then shifted forward by `block_size` samples.
///
/// A single forward-FFT plan and a single inverse-FFT plan (both sized
/// `2 * block_size`) are built once in the constructor and reused for every
/// call to `process_block` -- plans are never rebuilt per block.
pub struct PartitionedConvolver {
    block_size: usize,
    fft_size: usize,
    forward_fft: Arc<dyn Fft<f32>>,
    inverse_fft: Arc<dyn Fft<f32>>,
    /// Precomputed forward FFT of each zero-padded IR partition.
    partition_spectra: Vec<Vec<Complex<f32>>>,
    /// Running time-domain accumulator, holding not-yet-output overlap tail.
    /// Length is `(num_partitions + 1) * block_size`, which is large enough
    /// to hold the delayed contribution of the last partition in full.
    accumulator: Vec<f32>,
    /// Scratch buffer reused every call to avoid per-block allocation of the
    /// FFT input/output buffer.
    scratch: Vec<Complex<f32>>,
}

impl PartitionedConvolver {
    pub fn new(ir: &[f32], block_size: usize) -> Self {
        let block_size = block_size.max(1);
        let fft_size = 2 * block_size;

        let mut planner = FftPlanner::new();
        let forward_fft = planner.plan_fft_forward(fft_size);
        let inverse_fft = planner.plan_fft_inverse(fft_size);

        let num_partitions = ir.len().div_ceil(block_size).max(1);

        let mut partition_spectra = Vec::with_capacity(num_partitions);
        for k in 0..num_partitions {
            let start = k * block_size;
            let end = (start + block_size).min(ir.len());

            let mut buf = vec![Complex { re: 0.0, im: 0.0 }; fft_size];
            for (i, sample_idx) in (start..end).enumerate() {
                buf[i] = Complex {
                    re: ir[sample_idx],
                    im: 0.0,
                };
            }
            forward_fft.process(&mut buf);
            partition_spectra.push(buf);
        }

        let accumulator = vec![0.0; (num_partitions + 1) * block_size];

        Self {
            block_size,
            fft_size,
            forward_fft,
            inverse_fft,
            partition_spectra,
            accumulator,
            scratch: vec![Complex { re: 0.0, im: 0.0 }; fft_size],
        }
    }

    /// Processes exactly one `block_size`-length chunk of input audio and
    /// returns a `block_size`-length chunk of convolved output audio.
    ///
    /// Panics if `input.len() != block_size` -- callers are responsible for
    /// chunking their audio into fixed-size blocks matching the block size
    /// this convolver was constructed with.
    pub fn process_block(&mut self, input: &[f32]) -> Vec<f32> {
        assert_eq!(
            input.len(),
            self.block_size,
            "PartitionedConvolver::process_block requires input.len() == block_size"
        );

        // Zero-pad the incoming block to fft_size and take its forward FFT.
        for (i, sample) in input.iter().enumerate() {
            self.scratch[i] = Complex {
                re: *sample,
                im: 0.0,
            };
        }
        for slot in &mut self.scratch[input.len()..] {
            *slot = Complex { re: 0.0, im: 0.0 };
        }
        self.forward_fft.process(&mut self.scratch);
        let input_spectrum = self.scratch.clone();

        let num_partitions = self.partition_spectra.len();
        let inv_scale = 1.0 / self.fft_size as f32;

        for k in 0..num_partitions {
            // Pointwise complex multiplication (not convolution) between the
            // current block's spectrum and partition k's precomputed
            // spectrum.
            let mut product: Vec<Complex<f32>> = input_spectrum
                .iter()
                .zip(self.partition_spectra[k].iter())
                .map(|(a, b)| a * b)
                .collect();

            self.inverse_fft.process(&mut product);

            // Partition k's contribution lands k blocks in the future.
            let offset = k * self.block_size;
            for (i, c) in product.iter().enumerate() {
                // rustfft's inverse transform is unnormalized; scale by 1/N.
                self.accumulator[offset + i] += c.re * inv_scale;
            }
        }

        let output = self.accumulator[..self.block_size].to_vec();

        // Shift the accumulator forward by block_size samples, carrying the
        // not-yet-output tail forward and zero-filling the newly exposed end.
        let acc_len = self.accumulator.len();
        self.accumulator.copy_within(self.block_size..acc_len, 0);
        for slot in &mut self.accumulator[acc_len - self.block_size..] {
            *slot = 0.0;
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dead-simple O(n*m) direct time-domain convolution, used only as an
    /// independent reference to check the FFT-based implementation against.
    /// Deliberately not reused by `PartitionedConvolver` itself.
    fn direct_convolution(input: &[f32], ir: &[f32]) -> Vec<f32> {
        let mut output = vec![0.0f32; input.len() + ir.len() - 1];
        for (i, &x) in input.iter().enumerate() {
            for (j, &h) in ir.iter().enumerate() {
                output[i + j] += x * h;
            }
        }
        output
    }

    /// Runs `input` (assumed to be a multiple of `block_size` in length,
    /// zero-padded by the caller if needed) through the convolver one block
    /// at a time and returns the concatenated output.
    fn run_blocked(convolver: &mut PartitionedConvolver, input: &[f32], block_size: usize) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(block_size) {
            output.extend(convolver.process_block(chunk));
        }
        output
    }

    #[test]
    fn identity_plus_echo_impulse_response() {
        // ir = [1.0, 0.5]: identity plus a half-amplitude 1-sample echo.
        let ir = vec![1.0f32, 0.5];
        let block_size = 4;
        let mut convolver = PartitionedConvolver::new(&ir, block_size);

        // Impulse input, padded out to a few blocks so the full IR tail (and
        // the accumulator's carry-forward) is exercised.
        let num_blocks = 3;
        let mut input = vec![0.0f32; num_blocks * block_size];
        input[0] = 1.0;

        let actual = run_blocked(&mut convolver, &input, block_size);
        let expected_full = direct_convolution(&input, &ir);

        // direct_convolution's output length is input.len() + ir.len() - 1;
        // process_block only ever emits input.len() samples total, so
        // compare over the overlapping prefix (the discarded conv tail is
        // beyond the last output block: 1.0*0.5 landing at index input.len(),
        // which is inherently outside what this block-oriented API returns).
        assert_eq!(actual.len(), input.len());
        for (i, (&a, &e)) in actual.iter().zip(expected_full.iter()).enumerate() {
            assert!(
                (a - e).abs() < 1e-4,
                "mismatch at sample {i}: actual={a}, expected={e}"
            );
        }

        // Hand-computed sanity check of the first few samples:
        // y[0] = x[0]*ir[0] = 1.0*1.0 = 1.0
        // y[1] = x[0]*ir[1] = 1.0*0.5 = 0.5
        // y[2..] = 0.0
        assert!((actual[0] - 1.0).abs() < 1e-4, "actual[0] = {}", actual[0]);
        assert!((actual[1] - 0.5).abs() < 1e-4, "actual[1] = {}", actual[1]);
        for (i, &s) in actual.iter().enumerate().skip(2) {
            assert!(s.abs() < 1e-4, "actual[{i}] = {s}, expected ~0.0");
        }
    }

    #[test]
    fn multi_partition_ir_matches_direct_convolution() {
        // IR length 10 with block_size 4 needs ceil(10/4) = 3 partitions,
        // exercising the partition-accumulation delay-shift bookkeeping
        // beyond the trivial single-partition case.
        let ir: Vec<f32> = vec![1.0, 0.8, -0.5, 0.3, 0.2, -0.1, 0.05, 0.0, -0.02, 0.01];
        let block_size = 4;
        let mut convolver = PartitionedConvolver::new(&ir, block_size);

        // A short ramp signal, padded to a whole number of blocks with extra
        // trailing silent blocks so the full IR tail has landed in the
        // output before we stop feeding blocks.
        let num_blocks = 6;
        let mut input = vec![0.0f32; num_blocks * block_size];
        for (i, v) in [1.0, 0.5, -0.25, 0.75].iter().enumerate() {
            input[i] = *v;
        }

        let actual = run_blocked(&mut convolver, &input, block_size);
        let expected_full = direct_convolution(&input, &ir);

        assert_eq!(actual.len(), input.len());
        for (i, (&a, &e)) in actual.iter().zip(expected_full.iter()).enumerate() {
            assert!(
                (a - e).abs() < 1e-4,
                "mismatch at sample {i}: actual={a}, expected={e}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "process_block requires input.len() == block_size")]
    fn process_block_panics_on_wrong_length() {
        let ir = vec![1.0f32];
        let mut convolver = PartitionedConvolver::new(&ir, 4);
        convolver.process_block(&[0.0, 0.0]);
    }
}
