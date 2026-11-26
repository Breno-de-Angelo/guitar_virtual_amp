use std::time::{Duration, Instant};

const MIN_BPM: f32 = 30.0;
const MAX_BPM: f32 = 300.0;
/// Taps older than this are considered a new tapping sequence, not a
/// continuation of the previous one.
const TAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Tempo state driving the metronome UI panel: current BPM, running/stopped
/// transport, tap-tempo averaging, and beat-tick detection so time-based
/// pedals (Delay/Tremolo) can be synced to it. The audible click and the
/// beat lights are driven by the audio thread (`backend::capture`).
pub struct Metronome {
    pub bpm: f32,
    pub running: bool,
    tap_times: Vec<Instant>,
}

impl Default for Metronome {
    fn default() -> Self {
        Self::new()
    }
}

impl Metronome {
    pub fn new() -> Self {
        Self {
            bpm: 120.0,
            running: false,
            tap_times: Vec::new(),
        }
    }

    /// Records a tap and, once at least two taps are within `TAP_TIMEOUT` of
    /// each other, updates `bpm` to the average interval between them.
    pub fn tap(&mut self) {
        let now = Instant::now();
        self.tap_times
            .retain(|t| now.duration_since(*t) < TAP_TIMEOUT);
        self.tap_times.push(now);

        if self.tap_times.len() >= 2 {
            let intervals: Vec<f64> = self
                .tap_times
                .windows(2)
                .map(|w| w[1].duration_since(w[0]).as_secs_f64())
                .collect();
            let avg_secs = intervals.iter().sum::<f64>() / intervals.len() as f64;
            if avg_secs > 0.0 {
                self.bpm = (60.0 / avg_secs).clamp(MIN_BPM as f64, MAX_BPM as f64) as f32;
            }
        }
    }

    pub fn start(&mut self) {
        self.running = true;
    }

    pub fn stop(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn tap_tempo_computes_bpm_from_interval() {
        let mut metronome = Metronome::new();
        metronome.tap();
        sleep(Duration::from_millis(500));
        metronome.tap();

        // ~500ms between taps -> ~120 BPM.
        assert!(
            (metronome.bpm - 120.0).abs() < 10.0,
            "expected ~120 BPM, got {}",
            metronome.bpm
        );
    }

    #[test]
    fn single_tap_does_not_change_bpm() {
        let mut metronome = Metronome::new();
        let initial = metronome.bpm;
        metronome.tap();
        assert_eq!(metronome.bpm, initial);
    }
}
