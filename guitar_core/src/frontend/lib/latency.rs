//! Automatic latency calibration: the output callback plays a short pulse
//! (`AudioControls::request_pulse`) and records how many samples it had
//! sent to the UI at that moment. The pulse comes back through the
//! microphone or a loopback cable; the samples between the two are the
//! round trip that practice mode must compensate.
//!
//! Input and output samples are processed in the same callback, so output
//! sample `k` and input sample `k` share an index: a pulse played at `k`
//! that arrives at input index `k + n` took `n` samples.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::backend::controls::AudioControls;

/// Pulses per measurement.
const PULSES: usize = 6;
/// How long after a pulse to look for it.
const LISTEN_SECS: f64 = 0.6;
/// History kept for detection.
const HISTORY_SECS: f64 = 1.5;
/// Pause between pulses, so the room's echo dies down.
const GAP: Duration = Duration::from_millis(250);

/// Index (relative to `start`) of the pulse's onset in `samples[start..]`,
/// judged against the noise in the `noise_window` samples before `start`.
pub fn find_onset(samples: &[f32], start: usize, noise_window: usize) -> Option<usize> {
    let noise_from = start.saturating_sub(noise_window);
    let noise = &samples[noise_from..start.min(samples.len())];
    let floor = if noise.is_empty() {
        0.0
    } else {
        (noise.iter().map(|s| s * s).sum::<f32>() / noise.len() as f32).sqrt()
    };
    let threshold = (floor * 8.0).max(0.02);
    samples.get(start..)?.iter().position(|s| s.abs() > threshold)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProbeState {
    Idle,
    Running { done: usize, of: usize },
    /// Median round trip in seconds, and how many pulses were heard.
    Done { latency_secs: f64, heard: usize },
    Failed(String),
}

pub struct LatencyProbe {
    rate: f64,
    history: VecDeque<f32>,
    /// Absolute index of `history[0]` (samples received since start).
    history_start: u64,
    received: u64,
    running: bool,
    pending: Option<u64>,
    next_pulse_at: Option<Instant>,
    pulses_sent: usize,
    hits: Vec<f64>,
    state: ProbeState,
}

impl LatencyProbe {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            rate: sample_rate,
            history: VecDeque::new(),
            history_start: 0,
            received: 0,
            running: false,
            pending: None,
            next_pulse_at: None,
            pulses_sent: 0,
            hits: Vec::new(),
            state: ProbeState::Idle,
        }
    }

    pub fn state(&self) -> &ProbeState {
        &self.state
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// Starts a measurement. `received_so_far` counts every analysis sample
    /// the UI has drained since the app started, which lines up with
    /// `AudioControls::analysis_sent` on the audio side.
    pub fn start(&mut self, received_so_far: u64) {
        self.received = received_so_far;
        self.history.clear();
        self.history_start = self.received;
        self.running = true;
        self.pending = None;
        self.pulses_sent = 0;
        self.hits.clear();
        self.next_pulse_at = Some(Instant::now() + GAP);
        self.state = ProbeState::Running { done: 0, of: PULSES };
    }

    pub fn cancel(&mut self) {
        self.running = false;
        self.state = ProbeState::Idle;
    }

    /// Feeds the analysis samples that arrived this frame. Call every frame.
    pub fn feed(&mut self, samples: &[f32], controls: &AudioControls) {
        if !self.running {
            return;
        }
        self.history.extend(samples.iter().copied());
        self.received += samples.len() as u64;
        let max = (HISTORY_SECS * self.rate) as usize;
        if self.history.len() > max {
            let drop = self.history.len() - max;
            self.history.drain(..drop);
            self.history_start += drop as u64;
        }

        if self.pending.is_none()
            && let Some(at) = self.next_pulse_at
            && Instant::now() >= at
        {
            if self.pulses_sent >= PULSES {
                self.finish();
                return;
            }
            controls.request_pulse();
            self.pulses_sent += 1;
            self.pending = Some(u64::MAX);
            self.next_pulse_at = None;
        }
        if self.pending == Some(u64::MAX)
            && let Some(sent) = controls.pulse_sent_at()
        {
            self.pending = Some(sent);
        }
        if let Some(sent) = self.pending.filter(|s| *s != u64::MAX) {
            let listen = (LISTEN_SECS * self.rate) as u64;
            if self.received >= sent + listen {
                self.analyse(sent);
                self.pending = None;
                self.next_pulse_at = Some(Instant::now() + GAP);
                self.state = ProbeState::Running { done: self.pulses_sent, of: PULSES };
            }
        }
    }

    fn analyse(&mut self, sent: u64) {
        if sent < self.history_start {
            return;
        }
        let start = (sent - self.history_start) as usize;
        let samples: Vec<f32> = self.history.iter().copied().collect();
        let listen = (LISTEN_SECS * self.rate) as usize;
        let end = (start + listen).min(samples.len());
        if let Some(offset) = find_onset(&samples[..end], start, (0.1 * self.rate) as usize) {
            self.hits.push(offset as f64 / self.rate);
        }
    }

    fn finish(&mut self) {
        self.running = false;
        let heard = self.hits.len();
        self.state = match median(&mut self.hits) {
            Some(latency_secs) if heard * 2 >= PULSES => ProbeState::Done { latency_secs, heard },
            _ => ProbeState::Failed(format!(
                "Heard {heard} of {PULSES} pulses. Turn the speakers up, hold the microphone near them (or \
                 connect the output to the input with a cable) and try again."
            )),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onset_is_found_above_the_noise() {
        let mut samples = vec![0.001f32; 2000];
        for s in samples.iter_mut().skip(1500).take(50) {
            *s = 0.4;
        }
        assert_eq!(find_onset(&samples, 1000, 500), Some(500));
        assert_eq!(find_onset(&vec![0.001; 2000], 1000, 500), None);
    }

    #[test]
    fn measures_a_simulated_loopback() {
        let controls = AudioControls::default();
        let mut probe = LatencyProbe::new(1000.0);
        probe.start(0);
        let delay = 37; // samples
        let mut sent = 0u64;
        let mut echo_at: Option<u64> = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while probe.running() && Instant::now() < deadline {
            // What the audio thread would do for a 10-sample block.
            if controls.take_pulse_request() {
                controls.mark_pulse_sent(sent);
                echo_at = Some(sent + delay);
            }
            let block: Vec<f32> = (sent..sent + 10)
                .map(|i| if echo_at.is_some_and(|e| i >= e && i < e + 5) { 0.5 } else { 0.0 })
                .collect();
            sent += 10;
            controls.count_analysis_samples(10);
            probe.feed(&block, &controls);
            std::thread::sleep(Duration::from_millis(1));
        }
        match probe.state() {
            ProbeState::Done { latency_secs, heard } => {
                assert_eq!(*heard, PULSES);
                assert!((latency_secs - 0.037).abs() < 1e-9, "{latency_secs}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn silence_fails_with_advice() {
        let controls = AudioControls::default();
        let mut probe = LatencyProbe::new(1000.0);
        probe.start(0);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut sent = 0;
        while probe.running() && Instant::now() < deadline {
            if controls.take_pulse_request() {
                controls.mark_pulse_sent(sent);
            }
            sent += 10;
            controls.count_analysis_samples(10);
            probe.feed(&[0.0; 10], &controls);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(probe.state(), ProbeState::Failed(_)));
    }
}
