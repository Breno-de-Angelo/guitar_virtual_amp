use std::collections::VecDeque;

use num_complex::Complex;
use rustfft::FftPlanner;

pub fn compute_fft(buffer: &VecDeque<f64>) -> Vec<f64> {
    let n = buffer.len().next_power_of_two(); // tamanho da FFT
    if n == 0 {
        return Vec::new();
    }

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(n);

    let mut input: Vec<Complex<f64>> = buffer
        .iter()
        .cloned()
        .map(|x| Complex { re: x, im: 0.0 })
        .collect();
    input.resize(n, Complex { re: 0.0, im: 0.0 });

    let mut spectrum = input.clone();
    fft.process(&mut spectrum);

    spectrum[..n / 2].iter().map(|c| c.norm()).collect()
}
