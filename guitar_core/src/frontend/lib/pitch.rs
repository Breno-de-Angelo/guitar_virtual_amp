use std::collections::VecDeque;

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Just below a 5-string bass's low B (B0 ≈ 30.9 Hz is too low for the
/// tuner's window; E1 ≈ 41.2 Hz, a 4-string bass, is the lowest supported).
/// The search is capped at half the buffer, so short buffers simply can't
/// see the lowest notes.
const MIN_FREQ_HZ: f64 = 38.0;
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

/// Frequency of MIDI note `midi` with A4 (MIDI 69) at `a4_hz`.
pub fn midi_to_freq(midi: f64, a4_hz: f64) -> f64 {
    a4_hz * 2f64.powf((midi - 69.0) / 12.0)
}

/// Fractional MIDI note of a frequency.
pub fn freq_to_midi(freq: f64, a4_hz: f64) -> f64 {
    69.0 + 12.0 * (freq / a4_hz).log2()
}

/// Note name without octave, e.g. `"F#"`.
pub fn pitch_class_name(midi: i32) -> &'static str {
    NOTE_NAMES[midi.rem_euclid(12) as usize]
}

/// Note name with octave, e.g. `"E2"`.
pub fn note_name(midi: i32) -> String {
    format!("{}{}", pitch_class_name(midi), midi.div_euclid(12) - 1)
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

    /// `(note name, cents off)` of the nearest note.
    fn freq_to_note(freq: f64) -> (String, f64) {
        let midi = freq_to_midi(freq, 440.0);
        (note_name(midi.round() as i32), (midi - midi.round()) * 100.0)
    }

    #[test]
    fn reference_pitch_shifts_the_note_grid() {
        let midi = freq_to_midi(432.0, 432.0);
        assert_eq!(note_name(midi.round() as i32), "A4");
        // 440 Hz is ~+31.8 cents sharp of A when A4 = 432 Hz.
        let cents = (freq_to_midi(440.0, 432.0) - 69.0) * 100.0;
        assert!((cents - 31.77).abs() < 0.1, "{cents}");
        assert!((midi_to_freq(40.0, 440.0) - 82.407).abs() < 0.01);
        assert!((freq_to_midi(110.0, 440.0) - 45.0).abs() < 1e-9);
        assert_eq!(note_name(40), "E2");
        assert_eq!(pitch_class_name(-1), "B");
    }

    #[test]
    fn detects_low_bass_e_with_a_long_enough_buffer() {
        let buffer = sine_buffer(41.2, 24000.0, 2048);
        let detected = detect_pitch(&buffer, 24000.0).expect("should detect pitch");
        assert!((detected - 41.2).abs() < 0.5, "got {detected}");
    }

    #[test]
    fn a4_is_recognized_as_a() {
        let (name, cents) = freq_to_note(440.0);
        assert_eq!(name, "A4");
        assert!(cents.abs() < 0.01, "expected ~0 cents, got {cents}");
    }

    #[test]
    fn low_e_is_recognized() {
        let (name, cents) = freq_to_note(82.41);
        assert_eq!(name, "E2");
        assert!(cents.abs() < 1.0, "expected near 0 cents, got {cents}");
    }

    #[test]
    fn sharp_note_reports_positive_cents() {
        // 5 cents sharp of A4.
        let freq = 440.0 * 2f64.powf(5.0 / 1200.0);
        let (name, cents) = freq_to_note(freq);
        assert_eq!(name, "A4");
        assert!((cents - 5.0).abs() < 0.1, "expected ~+5 cents, got {cents}");
    }
}
