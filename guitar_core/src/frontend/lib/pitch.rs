use std::collections::VecDeque;

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

const MIN_FREQ_HZ: f64 = 70.0; // just below guitar low E (E2 ≈ 82.4 Hz)
const MAX_FREQ_HZ: f64 = 1000.0;
const SILENCE_RMS_THRESHOLD: f64 = 0.01;
/// YIN's "absolute threshold": the cumulative mean normalized difference must
/// dip below this for a lag to be accepted as the fundamental period, rather
/// than picking the global minimum (which is prone to octave errors on pure
/// tones where every multiple of the period looks equally good).
const YIN_THRESHOLD: f64 = 0.15;

/// Detects the fundamental frequency of `buffer` using the YIN algorithm
/// (cumulative mean normalized difference function + parabolic interpolation).
/// Returns `None` if the buffer is too quiet (no string being played) or too
/// short to analyze.
pub fn detect_pitch(buffer: &VecDeque<f64>, sample_rate: f32) -> Option<f64> {
    let samples: Vec<f64> = buffer.iter().cloned().collect();
    let n = samples.len();
    if n < 4 {
        return None;
    }

    let rms = (samples.iter().map(|s| s * s).sum::<f64>() / n as f64).sqrt();
    if rms < SILENCE_RMS_THRESHOLD {
        return None;
    }

    let sample_rate_f = sample_rate as f64;
    let min_tau = ((sample_rate_f / MAX_FREQ_HZ).floor() as usize).max(1);
    let max_tau = ((sample_rate_f / MIN_FREQ_HZ).ceil() as usize).min(n / 2);
    if min_tau >= max_tau {
        return None;
    }

    // Difference function: d(tau) = sum((x[i] - x[i+tau])^2).
    let diff_at = |tau: usize| -> f64 {
        let mut sum = 0.0;
        for i in 0..(n - tau) {
            let delta = samples[i] - samples[i + tau];
            sum += delta * delta;
        }
        sum
    };

    let mut d = vec![0.0; max_tau + 1];
    for (tau, item) in d.iter_mut().enumerate().take(max_tau + 1).skip(1) {
        *item = diff_at(tau);
    }

    // Cumulative mean normalized difference function: flattens the natural
    // downward trend of d(tau) so a threshold crossing means "this lag is a
    // period", independent of tau's magnitude.
    let mut cmnd = vec![1.0; max_tau + 1];
    let mut running_sum = 0.0;
    for tau in 1..=max_tau {
        running_sum += d[tau];
        cmnd[tau] = if running_sum > 0.0 {
            d[tau] * tau as f64 / running_sum
        } else {
            1.0
        };
    }

    // Walk lags in increasing order and take the first local minimum that
    // dips below the threshold -- this is the fundamental, not a harmonic.
    let mut chosen_tau = None;
    let mut tau = min_tau;
    while tau <= max_tau {
        if cmnd[tau] < YIN_THRESHOLD {
            while tau < max_tau && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            chosen_tau = Some(tau);
            break;
        }
        tau += 1;
    }

    // No lag dipped below threshold (e.g. noisy/inharmonic signal): fall back
    // to the global minimum in range, a looser guess rather than giving up.
    let best_tau = chosen_tau.unwrap_or_else(|| {
        (min_tau..=max_tau)
            .min_by(|&a, &b| cmnd[a].partial_cmp(&cmnd[b]).unwrap())
            .unwrap_or(min_tau)
    });

    // Parabolic interpolation around the chosen lag for sub-sample accuracy.
    let refined_tau = if best_tau > min_tau && best_tau < max_tau {
        let y_minus = cmnd[best_tau - 1];
        let y_zero = cmnd[best_tau];
        let y_plus = cmnd[best_tau + 1];
        let denom = y_minus - 2.0 * y_zero + y_plus;
        if denom.abs() > f64::EPSILON {
            let offset = 0.5 * (y_minus - y_plus) / denom;
            best_tau as f64 + offset.clamp(-1.0, 1.0)
        } else {
            best_tau as f64
        }
    } else {
        best_tau as f64
    };

    if refined_tau <= 0.0 {
        return None;
    }

    Some(sample_rate_f / refined_tau)
}

/// Converts a frequency in Hz to the nearest equal-temperament note (A4 = 440Hz),
/// returning `(note name, octave, cents deviation from that note)`.
pub fn freq_to_note(freq: f64) -> (&'static str, i32, f64) {
    let semitones_from_a4 = 12.0 * (freq / 440.0).log2();
    let midi_rounded = (69.0 + semitones_from_a4).round();
    let cents = (semitones_from_a4 - (midi_rounded - 69.0)) * 100.0;
    let midi = midi_rounded as i32;
    let name_idx = midi.rem_euclid(12) as usize;
    let octave = midi.div_euclid(12) - 1;
    (NOTE_NAMES[name_idx], octave, cents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine_buffer(freq: f64, sample_rate: f64, len: usize) -> VecDeque<f64> {
        (0..len)
            .map(|i| (2.0 * PI * freq * i as f64 / sample_rate).sin())
            .collect()
    }

    #[test]
    fn detects_known_sine_wave_frequency() {
        let sample_rate = 48000.0;
        let buffer = sine_buffer(220.0, sample_rate, 2048);
        let detected = detect_pitch(&buffer, sample_rate as f32).expect("should detect pitch");
        assert!(
            (detected - 220.0).abs() < 1.0,
            "expected ~220Hz, got {detected}"
        );
    }

    #[test]
    fn detects_low_e_string_frequency() {
        let sample_rate = 48000.0;
        let buffer = sine_buffer(82.41, sample_rate, 2048);
        let detected = detect_pitch(&buffer, sample_rate as f32).expect("should detect pitch");
        assert!(
            (detected - 82.41).abs() < 1.0,
            "expected ~82.41Hz, got {detected}"
        );
    }

    #[test]
    fn silence_returns_none() {
        let buffer: VecDeque<f64> = std::iter::repeat_n(0.0, 2048).collect();
        assert_eq!(detect_pitch(&buffer, 48000.0), None);
    }

    #[test]
    fn a4_is_recognized_as_a() {
        let (name, octave, cents) = freq_to_note(440.0);
        assert_eq!(name, "A");
        assert_eq!(octave, 4);
        assert!(cents.abs() < 0.01, "expected ~0 cents, got {cents}");
    }

    #[test]
    fn low_e_is_recognized() {
        let (name, octave, cents) = freq_to_note(82.41);
        assert_eq!(name, "E");
        assert_eq!(octave, 2);
        assert!(cents.abs() < 1.0, "expected near 0 cents, got {cents}");
    }

    #[test]
    fn sharp_note_reports_positive_cents() {
        // 5 cents sharp of A4.
        let freq = 440.0 * 2f64.powf(5.0 / 1200.0);
        let (name, _, cents) = freq_to_note(freq);
        assert_eq!(name, "A");
        assert!((cents - 5.0).abs() < 0.1, "expected ~+5 cents, got {cents}");
    }
}
