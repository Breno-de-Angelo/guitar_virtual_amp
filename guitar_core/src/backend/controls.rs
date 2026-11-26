//! Live controls and meters shared by the UI thread and the realtime output
//! callback (`backend::capture`).
//!
//! Everything here is a plain atomic: the audio thread reads the controls
//! once per callback and publishes peak levels, the UI writes the controls
//! and reads (and resets) the peaks once per frame. Floats are stored as
//! `f32::to_bits`. Peaks are non-negative, and for non-negative floats the
//! bit patterns sort like the values, so `fetch_max` on the bits keeps the
//! largest peak without a compare-and-swap loop.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

/// What the output callback sends to the recorder (see `backend::recorder`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordSource {
    /// The guitar after the pedal chain (no metronome, no tab-player song).
    Guitar,
    /// Everything that goes to the speakers: guitar, metronome and song.
    Mix,
}

#[derive(Debug)]
pub struct AudioControls {
    output_volume: AtomicU32,
    input_gain: AtomicU32,
    muted: AtomicBool,
    metronome_volume: AtomicU32,
    /// Beats per bar for the metronome accent; 0 or 1 = no accent.
    metronome_beats_per_bar: AtomicU32,
    /// Beat of the bar the metronome clicked last (0-based), for the UI.
    metronome_beat: AtomicU32,
    /// Incremented on every metronome click, so the UI can flash in sync
    /// with what is heard instead of keeping its own clock.
    metronome_clicks: AtomicU32,
    input_peak: AtomicU32,
    output_peak: AtomicU32,
    /// 0 = not recording, 1 = guitar, 2 = mix.
    record_mode: AtomicU8,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self {
            output_volume: AtomicU32::new(1.0f32.to_bits()),
            input_gain: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            metronome_volume: AtomicU32::new(1.0f32.to_bits()),
            metronome_beats_per_bar: AtomicU32::new(4),
            metronome_beat: AtomicU32::new(0),
            metronome_clicks: AtomicU32::new(0),
            input_peak: AtomicU32::new(0),
            output_peak: AtomicU32::new(0),
            record_mode: AtomicU8::new(0),
        }
    }
}

fn load_f32(atomic: &AtomicU32) -> f32 {
    f32::from_bits(atomic.load(Ordering::Relaxed))
}

fn store_f32(atomic: &AtomicU32, value: f32) {
    atomic.store(value.to_bits(), Ordering::Relaxed);
}

impl AudioControls {
    /// Master output volume, linear (1.0 = unity).
    pub fn output_volume(&self) -> f32 {
        load_f32(&self.output_volume)
    }
    pub fn set_output_volume(&self, volume: f32) {
        store_f32(&self.output_volume, volume.clamp(0.0, 4.0));
    }
    /// Gain applied to the guitar before the pedal chain, linear.
    pub fn input_gain(&self) -> f32 {
        load_f32(&self.input_gain)
    }
    pub fn set_input_gain(&self, gain: f32) {
        store_f32(&self.input_gain, gain.clamp(0.0, 8.0));
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
    /// Volume the speakers get: 0 while muted.
    pub fn effective_volume(&self) -> f32 {
        if self.muted() { 0.0 } else { self.output_volume() }
    }

    pub fn metronome_volume(&self) -> f32 {
        load_f32(&self.metronome_volume)
    }
    pub fn set_metronome_volume(&self, volume: f32) {
        store_f32(&self.metronome_volume, volume.clamp(0.0, 2.0));
    }
    pub fn metronome_beats_per_bar(&self) -> u32 {
        self.metronome_beats_per_bar.load(Ordering::Relaxed)
    }
    pub fn set_metronome_beats_per_bar(&self, beats: u32) {
        self.metronome_beats_per_bar.store(beats.min(16), Ordering::Relaxed);
    }
    /// `(beat of the bar, click count)` of the latest metronome click.
    pub fn metronome_position(&self) -> (u32, u32) {
        (
            self.metronome_beat.load(Ordering::Relaxed),
            self.metronome_clicks.load(Ordering::Relaxed),
        )
    }
    pub(crate) fn publish_metronome_click(&self, beat: u32) {
        self.metronome_beat.store(beat, Ordering::Relaxed);
        self.metronome_clicks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_input_peak(&self, peak: f32) {
        self.input_peak.fetch_max(peak.abs().to_bits(), Ordering::Relaxed);
    }
    pub(crate) fn record_output_peak(&self, peak: f32) {
        self.output_peak.fetch_max(peak.abs().to_bits(), Ordering::Relaxed);
    }
    /// Largest input sample (0..1, before the input gain) since the last call.
    pub fn take_input_peak(&self) -> f32 {
        f32::from_bits(self.input_peak.swap(0, Ordering::Relaxed))
    }
    /// Largest output sample (0..1, what the speakers get) since the last call.
    pub fn take_output_peak(&self) -> f32 {
        f32::from_bits(self.output_peak.swap(0, Ordering::Relaxed))
    }

    pub fn record_source(&self) -> Option<RecordSource> {
        match self.record_mode.load(Ordering::Relaxed) {
            1 => Some(RecordSource::Guitar),
            2 => Some(RecordSource::Mix),
            _ => None,
        }
    }
    pub fn set_record_source(&self, source: Option<RecordSource>) {
        let mode = match source {
            None => 0,
            Some(RecordSource::Guitar) => 1,
            Some(RecordSource::Mix) => 2,
        };
        self.record_mode.store(mode, Ordering::Relaxed);
    }
}

/// dB (full scale) to a linear gain.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear gain to dB, floored at -120 dB for silence.
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_keep_the_maximum_until_taken() {
        let controls = AudioControls::default();
        controls.record_input_peak(0.25);
        controls.record_input_peak(-0.75);
        controls.record_input_peak(0.5);
        assert_eq!(controls.take_input_peak(), 0.75);
        assert_eq!(controls.take_input_peak(), 0.0);
    }

    #[test]
    fn mute_zeroes_the_effective_volume() {
        let controls = AudioControls::default();
        controls.set_output_volume(0.5);
        assert_eq!(controls.effective_volume(), 0.5);
        controls.set_muted(true);
        assert_eq!(controls.effective_volume(), 0.0);
        assert_eq!(controls.output_volume(), 0.5);
    }

    #[test]
    fn record_source_round_trips() {
        let controls = AudioControls::default();
        assert_eq!(controls.record_source(), None);
        controls.set_record_source(Some(RecordSource::Mix));
        assert_eq!(controls.record_source(), Some(RecordSource::Mix));
        controls.set_record_source(None);
        assert_eq!(controls.record_source(), None);
    }

    #[test]
    fn decibel_conversions_invert() {
        assert!((db_to_gain(-6.0) - 0.501).abs() < 0.001);
        assert!((gain_to_db(db_to_gain(3.5)) - 3.5).abs() < 1e-4);
        assert_eq!(gain_to_db(0.0), -120.0);
    }
}
