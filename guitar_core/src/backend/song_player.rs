//! Song playback engine for the tab player: a SoundFont synthesizer driven
//! by the score's note events, plus an optional backing track (an audio file
//! the user loaded, or the one embedded in a Guitar Pro 8 file).
//!
//! ## Threading
//!
//! The engine lives behind an `Arc<Mutex<_>>` so it survives output-stream
//! rebuilds (device or buffer-size changes). The realtime output callback
//! only ever calls `try_lock`: if the UI thread happens to hold the lock, the
//! song is silent for that one callback, but the guitar signal is never
//! delayed. UI-side calls hold the lock only for quick state changes;
//! expensive work (decoding audio, building synthesizers, building the
//! timeline) is done before taking it. Playback position is published
//! through atomics so the UI can draw the cursor every frame without locking.
//!
//! Both the synth and the backing track are advanced by the same
//! output-sample clock, so they cannot drift apart from each other.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::backend::audio_file::DecodedAudio;
use crate::shared::tab::timeline::{SyncMap, Timeline};

/// Frames rendered between event checks: note timing resolution is
/// `CHUNK_FRAMES / sample_rate` (1.3 ms at 48 kHz).
const CHUNK_FRAMES: usize = 64;

/// Per-channel synth setup sent once per song: `(channel, program, volume)`.
pub type ChannelSetup = Vec<(u8, u8, u8)>;

/// Read-only playback state published by the audio thread.
#[derive(Debug, Default)]
pub struct PlayerStatus {
    playing: AtomicBool,
    score_secs: AtomicU64,
    audio_secs: AtomicU64,
    output_rate: AtomicU32,
}

impl PlayerStatus {
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    /// Current position in score time (see `shared::tab::timeline`).
    pub fn score_secs(&self) -> f64 {
        f64::from_bits(self.score_secs.load(Ordering::Relaxed))
    }
    /// Current position in the backing track, in seconds.
    pub fn audio_secs(&self) -> f64 {
        f64::from_bits(self.audio_secs.load(Ordering::Relaxed))
    }
    pub fn output_rate(&self) -> u32 {
        self.output_rate.load(Ordering::Relaxed)
    }
}

pub struct PlayerEngine {
    output_rate: u32,
    soundfont: Option<Arc<SoundFont>>,
    synth: Option<Synthesizer>,
    channel_setup: ChannelSetup,
    timeline: Option<Arc<Timeline>>,
    track_enabled: Vec<bool>,
    backing: Option<Arc<DecodedAudio>>,
    use_backing: bool,
    sync: SyncMap,
    playing: bool,
    score_secs: f64,
    audio_secs: f64,
    speed: f64,
    next_event: usize,
    synth_gain: f32,
    backing_gain: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    status: Arc<PlayerStatus>,
}

impl PlayerEngine {
    fn new(status: Arc<PlayerStatus>) -> Self {
        Self {
            output_rate: 48_000,
            soundfont: None,
            synth: None,
            channel_setup: Vec::new(),
            timeline: None,
            track_enabled: Vec::new(),
            backing: None,
            use_backing: true,
            sync: SyncMap::default(),
            playing: false,
            score_secs: 0.0,
            audio_secs: 0.0,
            speed: 1.0,
            next_event: 0,
            synth_gain: 1.0,
            backing_gain: 1.0,
            left: vec![0.0; CHUNK_FRAMES],
            right: vec![0.0; CHUNK_FRAMES],
            status,
        }
    }

    fn backing_active(&self) -> bool {
        self.use_backing && self.backing.is_some()
    }

    fn apply_channel_setup(&mut self) {
        let Some(synth) = &mut self.synth else { return };
        synth.reset();
        for &(channel, program, volume) in &self.channel_setup {
            let ch = channel as i32;
            synth.process_midi_message(ch, 0xC0, program as i32, 0);
            synth.process_midi_message(ch, 0xB0, 7, volume as i32);
        }
    }

    fn silence(&mut self) {
        if let Some(synth) = &mut self.synth {
            synth.note_off_all(false);
        }
    }

    /// Moves the playhead to `score_secs`, keeping the backing track aligned.
    fn seek_score(&mut self, score_secs: f64) {
        self.score_secs = score_secs.max(0.0);
        if self.backing_active() {
            self.audio_secs = self.sync.score_to_audio(self.score_secs).max(0.0);
            // When the song starts before the audio offset, start at audio 0.
            self.score_secs = self.sync.audio_to_score(self.audio_secs);
        } else {
            self.audio_secs = self.sync.score_to_audio(self.score_secs).max(0.0);
        }
        self.resync_events();
        self.publish();
    }

    fn resync_events(&mut self) {
        self.silence();
        self.next_event = self
            .timeline
            .as_ref()
            .map(|t| t.events.partition_point(|e| e.secs < self.score_secs))
            .unwrap_or(0);
    }

    fn publish(&self) {
        self.status.playing.store(self.playing, Ordering::Relaxed);
        self.status.score_secs.store(self.score_secs.to_bits(), Ordering::Relaxed);
        self.status.audio_secs.store(self.audio_secs.to_bits(), Ordering::Relaxed);
    }

    fn end_secs(&self) -> f64 {
        let score_end = self.timeline.as_ref().map(|t| t.duration_secs).unwrap_or(0.0);
        // Let the last notes ring out briefly before stopping.
        let score_end = score_end + 1.0;
        match (&self.backing, self.backing_active()) {
            (Some(b), true) => self.sync.audio_to_score(b.duration_secs()).max(score_end),
            _ => score_end,
        }
    }

    /// Mixes the next `out.len()` mono frames of song audio into `out`.
    /// Called from the realtime output callback.
    pub fn render(&mut self, out: &mut [f32]) {
        if !self.playing || self.timeline.is_none() {
            return;
        }
        let rate = self.output_rate.max(1) as f64;
        let end = self.end_secs();
        for chunk in out.chunks_mut(CHUNK_FRAMES) {
            let n = chunk.len();
            let dt = n as f64 / rate;
            let audio_start = self.audio_secs;
            let new_score = if self.backing_active() {
                self.audio_secs += dt;
                self.sync.audio_to_score(self.audio_secs)
            } else {
                let score = self.score_secs + dt * self.speed;
                self.audio_secs = self.sync.score_to_audio(score).max(0.0);
                score
            };

            if let (Some(timeline), Some(synth)) = (&self.timeline, &mut self.synth) {
                let events = &timeline.events;
                while let Some(ev) = events.get(self.next_event) {
                    if ev.secs >= new_score {
                        break;
                    }
                    if ev.velocity == 0 {
                        synth.note_off(ev.channel as i32, ev.key as i32);
                    } else if self.track_enabled.get(ev.track as usize).copied().unwrap_or(true) {
                        synth.note_on(ev.channel as i32, ev.key as i32, ev.velocity as i32);
                    }
                    self.next_event += 1;
                }
                synth.render(&mut self.left[..n], &mut self.right[..n]);
                let gain = self.synth_gain * 0.5;
                for (i, sample) in chunk.iter_mut().enumerate() {
                    *sample += (self.left[i] + self.right[i]) * gain;
                }
            }
            self.score_secs = new_score;

            if self.backing_active()
                && let Some(backing) = &self.backing
            {
                for (i, sample) in chunk.iter_mut().enumerate() {
                    *sample += backing.sample_at(audio_start + i as f64 / rate) * self.backing_gain;
                }
            }

            if self.score_secs >= end {
                self.playing = false;
                self.silence();
                break;
            }
        }
        self.publish();
    }
}

/// Cloneable handle shared by the UI and the audio callback.
#[derive(Clone)]
pub struct SongPlayer {
    engine: Arc<Mutex<PlayerEngine>>,
    status: Arc<PlayerStatus>,
}

impl Default for SongPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SongPlayer {
    pub fn new() -> Self {
        let status = Arc::new(PlayerStatus::default());
        status.output_rate.store(48_000, Ordering::Relaxed);
        Self { engine: Arc::new(Mutex::new(PlayerEngine::new(status.clone()))), status }
    }

    pub fn status(&self) -> &PlayerStatus {
        &self.status
    }

    fn with<R>(&self, f: impl FnOnce(&mut PlayerEngine) -> R) -> R {
        let mut engine = self.engine.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut engine)
    }

    /// Realtime-safe render entry point for the output callback: never
    /// blocks, and leaves `out` untouched if the engine is busy.
    pub fn try_render(&self, out: &mut [f32]) {
        if let Ok(mut engine) = self.engine.try_lock() {
            engine.render(out);
        }
    }

    /// Called whenever an output stream is (re)built for a device.
    pub fn set_output_rate(&self, rate: u32) {
        let rate = rate.max(1);
        self.status.output_rate.store(rate, Ordering::Relaxed);
        let soundfont = self.with(|e| {
            if e.output_rate == rate {
                return None;
            }
            e.output_rate = rate;
            e.soundfont.clone()
        });
        if let Some(sf) = soundfont {
            self.set_soundfont(sf);
        }
    }

    /// Installs a SoundFont, building a synthesizer for the current output
    /// rate outside the lock.
    pub fn set_soundfont(&self, soundfont: Arc<SoundFont>) {
        let rate = self.status.output_rate();
        let mut settings = SynthesizerSettings::new(rate as i32);
        settings.block_size = CHUNK_FRAMES;
        let synth = match Synthesizer::new(&soundfont, &settings) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Falha ao criar o sintetizador: {e:?}");
                return;
            }
        };
        let old = self.with(|e| {
            e.soundfont = Some(soundfont);
            let old = e.synth.replace(synth);
            e.apply_channel_setup();
            e.resync_events();
            old
        });
        // Dropped here, on the caller's thread, not on the audio thread.
        drop(old);
    }

    pub fn has_synth(&self) -> bool {
        self.with(|e| e.synth.is_some())
    }

    /// Loads a new song, stopping playback and rewinding to the start.
    pub fn set_song(&self, timeline: Arc<Timeline>, channels: ChannelSetup, track_count: usize) {
        let old = self.with(|e| {
            e.playing = false;
            e.channel_setup = channels;
            e.track_enabled = vec![true; track_count];
            let old = e.timeline.replace(timeline);
            e.apply_channel_setup();
            e.seek_score(0.0);
            old
        });
        drop(old);
    }

    pub fn set_backing(&self, backing: Option<Arc<DecodedAudio>>, sync: SyncMap) {
        let old = self.with(|e| {
            let old = std::mem::replace(&mut e.backing, backing);
            e.sync = sync;
            let pos = e.score_secs;
            e.seek_score(pos);
            old
        });
        drop(old);
    }

    /// Changes the score/audio alignment, keeping the audio position when a
    /// backing track is playing (the cursor jumps, the music doesn't).
    pub fn set_sync(&self, sync: SyncMap) {
        self.with(|e| {
            e.sync = sync;
            if e.backing_active() {
                e.score_secs = e.sync.audio_to_score(e.audio_secs);
                e.resync_events();
                e.publish();
            }
        });
    }

    pub fn set_use_backing(&self, use_backing: bool) {
        self.with(|e| {
            if e.use_backing != use_backing {
                e.use_backing = use_backing;
                let pos = e.score_secs;
                e.seek_score(pos);
            }
        });
    }

    pub fn play(&self) {
        self.with(|e| {
            if e.timeline.is_none() {
                return;
            }
            if e.score_secs >= e.end_secs() - 1.0 {
                e.seek_score(0.0);
            }
            e.playing = true;
            e.publish();
        });
    }

    pub fn pause(&self) {
        self.with(|e| {
            e.playing = false;
            e.silence();
            e.publish();
        });
    }

    pub fn stop(&self) {
        self.with(|e| {
            e.playing = false;
            e.seek_score(0.0);
        });
    }

    pub fn seek(&self, score_secs: f64) {
        self.with(|e| e.seek_score(score_secs));
    }

    /// Moves to a backing-track position, even one before the first bar
    /// (e.g. to listen to the intro while tapping the sync point).
    pub fn seek_audio(&self, audio_secs: f64) {
        self.with(|e| {
            e.audio_secs = audio_secs.max(0.0);
            e.score_secs = e.sync.audio_to_score(e.audio_secs);
            e.resync_events();
            e.publish();
        });
    }

    /// Playback speed for synth-only playback (the backing track always
    /// plays at its own speed).
    pub fn set_speed(&self, speed: f64) {
        self.with(|e| e.speed = speed.clamp(0.25, 2.0));
    }

    pub fn set_track_enabled(&self, enabled: Vec<bool>) {
        self.with(|e| {
            if let (Some(timeline), Some(synth)) = (&e.timeline, &mut e.synth) {
                for (track, (&now, &before)) in enabled.iter().zip(&e.track_enabled).enumerate() {
                    if before && !now
                        && let Some(&ch) = timeline.track_channels.get(track)
                    {
                        synth.note_off_all_channel(ch as i32, false);
                    }
                }
            }
            e.track_enabled = enabled;
        });
    }

    pub fn set_gains(&self, synth_gain: f32, backing_gain: f32) {
        self.with(|e| {
            e.synth_gain = synth_gain.max(0.0);
            e.backing_gain = backing_gain.max(0.0);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::model::{MasterBar, Score};

    /// Two 4/4 bars at 120 bpm: 4 seconds of score, no notes.
    fn two_bar_timeline() -> Arc<Timeline> {
        let bar = MasterBar { numerator: 4, denominator: 4, ..Default::default() };
        let score = Score { initial_tempo: 120.0, bars: vec![bar.clone(), bar], ..Default::default() };
        Arc::new(Timeline::build(&score))
    }

    fn player_at(rate: u32) -> SongPlayer {
        let player = SongPlayer::new();
        player.set_output_rate(rate);
        player.set_song(two_bar_timeline(), Vec::new(), 0);
        player
    }

    #[test]
    fn synth_only_clock_follows_speed() {
        let player = player_at(1000);
        player.set_speed(0.5);
        player.play();
        let mut buf = vec![0.0; 1000];
        player.try_render(&mut buf);
        assert!((player.status().score_secs() - 0.5).abs() < 1e-9);
        assert!(player.status().playing());
    }

    #[test]
    fn playback_stops_after_the_end() {
        let player = player_at(1000);
        player.play();
        let mut buf = vec![0.0; 1000];
        for _ in 0..6 {
            player.try_render(&mut buf);
        }
        assert!(!player.status().playing());
        // Play again from the end restarts at the beginning.
        player.play();
        assert_eq!(player.status().score_secs(), 0.0);
    }

    #[test]
    fn backing_track_is_mixed_with_sync_offset() {
        let player = player_at(1000);
        // 3 seconds of constant 0.5 at 1 kHz.
        let backing = Arc::new(DecodedAudio { samples: vec![0.5; 3000], sample_rate: 1000 });
        // Bar 1 starts 1s into the recording.
        player.set_backing(Some(backing), SyncMap::Linear { offset_secs: 1.0, tempo_ratio: 1.0 });
        player.set_gains(1.0, 0.5);
        player.play();
        // Starting at score 0 means audio position 1s.
        assert!((player.status().audio_secs() - 1.0).abs() < 1e-9);
        let mut buf = vec![0.0; 500];
        player.try_render(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!((player.status().score_secs() - 0.5).abs() < 1e-9);
        assert!((player.status().audio_secs() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn changing_sync_moves_cursor_not_audio() {
        let player = player_at(1000);
        let backing = Arc::new(DecodedAudio { samples: vec![0.0; 10_000], sample_rate: 1000 });
        player.set_backing(Some(backing), SyncMap::default());
        player.seek(2.0);
        player.set_sync(SyncMap::Linear { offset_secs: 0.5, tempo_ratio: 1.0 });
        assert!((player.status().audio_secs() - 2.0).abs() < 1e-9);
        assert!((player.status().score_secs() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn disabling_backing_falls_back_to_synth_clock() {
        let player = player_at(1000);
        let backing = Arc::new(DecodedAudio { samples: vec![1.0; 10_000], sample_rate: 1000 });
        player.set_backing(Some(backing), SyncMap::default());
        player.set_use_backing(false);
        player.play();
        let mut buf = vec![0.0; 100];
        player.try_render(&mut buf);
        assert!(buf.iter().all(|s| *s == 0.0));
    }
}
