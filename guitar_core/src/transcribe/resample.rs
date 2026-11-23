//! Offline sample-rate conversion (windowed sinc), for feeding decoded
//! audio at any rate to models that expect a fixed one.

const ZERO_CROSSINGS: f64 = 16.0;

fn blackman(x: f64) -> f64 {
    // x in [-1, 1]
    let t = (x + 1.0) * 0.5;
    0.42 - 0.5 * (std::f64::consts::TAU * t).cos() + 0.08 * (2.0 * std::f64::consts::TAU * t).cos()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Most filter phases precomputed by [`resample`] (one per distinct output
/// position between two input samples).
const MAX_PHASES: u64 = 4096;

/// Converts `input` from `from` Hz to `to` Hz. The low-pass sits just below
/// the lower of the two Nyquist frequencies, so downsampling doesn't alias.
/// Common rate pairs (44.1/48 kHz to 16/22.05 kHz...) use a polyphase table
/// of the same windowed-sinc filter, about 20x faster than evaluating it
/// per sample.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let g = gcd(from as u64, to as u64);
    let (up, down) = (to as u64 / g, from as u64 / g);
    if up > MAX_PHASES {
        return resample_direct(input, from, to);
    }
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0) * 0.95;
    let half_width = ZERO_CROSSINGS / cutoff;
    // Output i sits at input position i * down / up = base + phase / up.
    // Taps for phase p cover input offsets -reach..=reach around base.
    let reach = half_width.ceil() as i64 + 1;
    let taps = (2 * reach + 1) as usize;
    let table: Vec<f32> = (0..up)
        .flat_map(|p| {
            let frac = p as f64 / up as f64;
            (-reach..=reach).map(move |k| {
                let d = k as f64 - frac;
                if d.abs() > half_width {
                    0.0
                } else {
                    (cutoff * sinc(d * cutoff) * blackman(d / half_width)) as f32
                }
            })
        })
        .collect();
    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let n = input.len() as i64;
    (0..out_len as u64)
        .map(|i| {
            let pos = i * down;
            let base = (pos / up) as i64;
            let phase = (pos % up) as usize;
            let coeffs = &table[phase * taps..(phase + 1) * taps];
            let first = base - reach;
            if first >= 0 && first + taps as i64 <= n {
                let window = &input[first as usize..first as usize + taps];
                window.iter().zip(coeffs).map(|(x, c)| x * c).sum::<f32>()
            } else {
                coeffs
                    .iter()
                    .enumerate()
                    .filter_map(|(k, c)| {
                        let j = first + k as i64;
                        (0..n).contains(&j).then(|| input[j as usize] * c)
                    })
                    .sum()
            }
        })
        .collect()
}

/// Same filter evaluated per output sample, for rate pairs with too many
/// phases to tabulate.
fn resample_direct(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    let ratio = to as f64 / from as f64;
    // Cutoff relative to the input rate (1.0 = input Nyquist).
    let cutoff = ratio.min(1.0) * 0.95;
    let half_width = ZERO_CROSSINGS / cutoff;
    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let center = i as f64 / ratio;
        let first = (center - half_width).ceil().max(0.0) as usize;
        let last = ((center + half_width).floor() as usize).min(input.len() - 1);
        let mut acc = 0.0;
        for (j, &x) in input.iter().enumerate().take(last + 1).skip(first) {
            let d = j as f64 - center;
            acc += x as f64 * cutoff * sinc(d * cutoff) * blackman(d / half_width);
        }
        out.push(acc as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        (0..(secs * rate as f64) as usize)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn amplitude_at(signal: &[f32], freq: f64, rate: u32) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &x) in signal.iter().enumerate() {
            let phase = std::f64::consts::TAU * freq * i as f64 / rate as f64;
            re += x as f64 * phase.cos();
            im += x as f64 * phase.sin();
        }
        2.0 * (re * re + im * im).sqrt() / signal.len() as f64
    }

    #[test]
    fn keeps_in_band_tones_and_length() {
        let out = resample(&sine(440.0, 44_100, 1.0), 44_100, 22_050);
        assert_eq!(out.len(), 22_050);
        let a = amplitude_at(&out[1000..21000], 440.0, 22_050);
        assert!((a - 1.0).abs() < 0.02, "amplitude {a}");
    }

    #[test]
    fn removes_tones_above_the_new_nyquist() {
        // 15 kHz is fine at 48 kHz but would alias to 7.05 kHz at 22.05 kHz.
        let out = resample(&sine(15_000.0, 48_000, 0.5), 48_000, 22_050);
        let aliased = amplitude_at(&out[500..10_000], 22_050.0 - 15_000.0, 22_050);
        assert!(aliased < 0.01, "aliased amplitude {aliased}");
    }

    #[test]
    fn polyphase_matches_the_direct_filter() {
        let input: Vec<f32> = (0..20_000)
            .map(|i| ((i as f32 * 0.013).sin() + 0.3 * (i as f32 * 0.31).sin()) * 0.5)
            .collect();
        for (from, to) in [
            (44_100, 16_000),
            (48_000, 22_050),
            (22_050, 16_000),
            (8000, 16_000),
        ] {
            let fast = resample(&input, from, to);
            let slow = resample_direct(&input, from, to);
            assert_eq!(fast.len(), slow.len());
            let err = fast
                .iter()
                .zip(&slow)
                .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(err < 1e-4, "{from}->{to}: {err}");
        }
    }

    #[test]
    fn upsampling_and_identity_work() {
        let input = sine(100.0, 8000, 0.25);
        assert_eq!(resample(&input, 8000, 8000), input);
        let up = resample(&input, 8000, 16_000);
        assert_eq!(up.len(), 4000);
        let a = amplitude_at(&up[400..3600], 100.0, 16_000);
        assert!((a - 1.0).abs() < 0.02, "amplitude {a}");
    }
}
