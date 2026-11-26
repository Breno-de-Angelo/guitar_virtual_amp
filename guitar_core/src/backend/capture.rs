//! Real-time audio bridge between the input (capture) and output (playback) cpal streams.
//!
//! ## Risk assessment: per-sample crossbeam channels + wholesale `PedalChain` rebuild
//!
//! This module bridges input and output audio callbacks with `crossbeam::channel::bounded`,
//! sending one message *per sample* (48,000/sec at the configured sample rate) in each
//! direction, and the output callback rebuilds the entire `PedalChain` from scratch whenever
//! a chain-description update arrives on `pedal_rx`.
//!
//! On paper a "channel per sample" design sounds like a red flag for a realtime audio path:
//! `crossbeam::bounded` uses a `Mutex`-guarded ring buffer internally, and its blocking
//! `send`/`recv` can park the calling thread — exactly what you don't want inside an audio
//! callback deadline. However, auditing the *actual* call sites here (not just the channel
//! type) changes the picture:
//!
//! - The output callback (`build_output_stream`) never calls blocking `send`/`recv`. It only
//!   uses `pedal_rx.try_iter()` and `input_rx.try_recv()` (draining) and `audio_tx.try_send()`
//!   (best-effort, drops on backpressure). All three return immediately regardless of channel
//!   state — no lock-park/wait behavior reaches the realtime output thread. Rebuilding
//!   `PedalChain::from_description` only happens when an update is actually pending in
//!   `pedal_rx`, i.e. at most once per UI-driven edit, not per sample — its cost is a `Vec`
//!   allocation plus each pedal's (cheap, fixed-size) constructor, not per-callback overhead.
//! - The one blocking call found was `input_tx.send(sample)` in the *input* stream callback
//!   (producer side). With input and output streams reading/writing at the same sample rate
//!   and a 4096-sample bounded capacity (~85ms of headroom at 48kHz), this essentially never
//!   blocks in steady state, but it is a real (if unlikely) glitch vector if the output thread
//!   is ever stalled/descheduled for longer than the buffer holds. This has been changed to
//!   `try_send` (dropping the sample rather than blocking the capture callback) below, which
//!   costs nothing in the common case and removes the only latent blocking call in either
//!   audio-thread hot path.
//!
//! ## Why not swap to `rtrb` (lock-free SPSC ring buffer)
//!
//! `rtrb` was investigated as a replacement for the input→output sample channel. It would
//! shave a small constant-factor amount of overhead off `try_recv`/`try_send` (no mutex at
//! all vs. crossbeam's mutex-guarded ring buffer), but two things make it not worth doing
//! here: (1) the audit above shows the hot path is already non-blocking/wait-free in
//! practice — there's no lock contention to remove, just an uncontended mutex acquisition;
//! (2) `input_tx`/`pedal_tx`/`input_rx`/`pedal_rx`/`audio_tx` are part of the public
//! `AudioSetup` API and are consumed directly as `crossbeam::channel` `Sender`/`Receiver`
//! values by `guitar_core::frontend::gui` (it clones `input_tx` and calls `.send()` on it
//! directly when rebuilding an input stream on device switch, and `switch_output_device`'s
//! signature is typed in terms of these channel types). Swapping the sample channel to
//! `rtrb::Producer`/`Consumer` would require changing that call site too, which is out of
//! scope for this change (scoped to `capture.rs`) and not justified by the risk level found.
//! If profiling ever shows measurable contention on this channel, `rtrb` remains the natural
//! next step.
//!
//! ## Instrumentation
//!
//! Two pieces of data are now exposed on `AudioSetup` for a future UI overlay:
//!
//! - `AudioSetup::configured_latency_secs` — the nominal round-trip buffer latency in seconds
//!   (`GLOBAL_CONFIG.buffer_size / GLOBAL_CONFIG.sample_rate`), computed once at stream setup.
//! - `AudioSetup::last_output_callback_ns` — an `Arc<AtomicU64>` updated at the end of every
//!   output-stream callback with its wall-clock duration in nanoseconds (measured via
//!   `std::time::Instant` at the top/bottom of the callback closure). Read it with
//!   `.load(Ordering::Relaxed)`; a UI overlay can poll this each frame and compare it against
//!   the callback's expected period (`buffer_size / sample_rate`) to flag when processing is
//!   eating into the realtime budget. It's cheap (one atomic store per callback, no
//!   allocation) and safe to read from any thread.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use crate::backend::controls::{AudioControls, RecordSource};
use crate::backend::pedals::pedal::{MultiChannelPedalChain, PedalChain};
use crate::backend::recorder::record_channel;
use crate::backend::song_player::SongPlayer;
use crate::shared::pedals::PedalInstance;
use crate::shared::config::GLOBAL_CONFIG;

/// Samples waiting in the input→output queue at the start of the latest
/// output callback, i.e. latency added on top of the device buffers. A
/// static rather than an `AudioSetup` field because output streams get
/// rebuilt on device/buffer changes and only one exists at a time; the UI
/// meter reads it.
pub static INPUT_BACKLOG_SAMPLES: AtomicU32 = AtomicU32::new(0);

/// How long the backlog trimmer watches before deciding (~0.5 s at 48 kHz).
const BACKLOG_WINDOW_SAMPLES: usize = 24_000;
/// Extra queued audio the trimmer always leaves, to absorb callback jitter.
const BACKLOG_MARGIN_SAMPLES: usize = 64;

/// Keeps the input→output queue from holding more audio than it needs.
///
/// Input and output callbacks run at the same nominal rate, so anything
/// that piles up in the queue (a slow callback, a chain rebuild, the output
/// stream starting after the input, or two devices whose clocks drift
/// apart) never drains by itself and stays as extra latency, up to the
/// queue's 85 ms capacity. The trimmer tracks the smallest surplus seen
/// over a window: if even the leanest callback had `surplus` samples more
/// than it needed, those were pure delay, so all but a safety margin are
/// dropped. Dropping makes a small click, but only after audio actually
/// piled up.
#[derive(Default)]
struct BacklogTrimmer {
    min_surplus: Option<usize>,
    window_samples: usize,
}

impl BacklogTrimmer {
    /// Call at the start of each output callback with the queue length and
    /// the frames this callback will consume. Returns how many queued
    /// samples to drop.
    fn samples_to_drop(&mut self, queued: usize, frames: usize) -> usize {
        let surplus = queued.saturating_sub(frames);
        self.min_surplus = Some(self.min_surplus.map_or(surplus, |m| m.min(surplus)));
        self.window_samples += frames;
        if self.window_samples < BACKLOG_WINDOW_SAMPLES {
            return 0;
        }
        let excess = self.min_surplus.unwrap_or(0).saturating_sub(BACKLOG_MARGIN_SAMPLES);
        *self = Self::default();
        excess
    }
}

/// Largest block the output callback processes at once (matches the
/// biggest buffer size the UI offers); longer callbacks are split.
const OUTPUT_BLOCK_SAMPLES: usize = 4096;

/// State the output callback owns: chain, metronome, scratch buffer.
struct OutputRenderer {
    pedal_chain: PedalChain,
    metronome_click: MetronomeClick,
    trimmer: BacklogTrimmer,
    block: Vec<i16>,
    /// The block before the chain (for the dry analysis tap).
    dry: Vec<i16>,
    controls: Arc<AudioControls>,
    /// Samples of the latency-test pulse still to play.
    pulse_remaining: u32,
}

/// Latency-test pulse: a 2 ms, 2 kHz burst at half scale.
const PULSE_SAMPLES: u32 = 96;
const PULSE_FREQ_HZ: f32 = 2000.0;
const PULSE_GAIN: f32 = 0.5;

impl OutputRenderer {
    fn new(pedal_chain: PedalChain, metronome_click: MetronomeClick, controls: Arc<AudioControls>) -> Self {
        Self {
            pedal_chain,
            metronome_click,
            trimmer: BacklogTrimmer::default(),
            block: vec![0; OUTPUT_BLOCK_SAMPLES],
            dry: vec![0; OUTPUT_BLOCK_SAMPLES],
            controls,
            pulse_remaining: 0,
        }
    }

}

/// Next sample of the latency-test pulse (added to the click).
fn next_pulse_sample(remaining: &mut u32) -> i16 {
    {
        if *remaining == 0 {
            return 0;
        }
        let i = (PULSE_SAMPLES - *remaining) as f32;
        *remaining -= 1;
        let phase = i * std::f32::consts::TAU * PULSE_FREQ_HZ / GLOBAL_CONFIG.sample_rate;
        (phase.sin() * PULSE_GAIN * i16::MAX as f32) as i16
    }
}

impl OutputRenderer {

    /// Renders `frames` mono samples, calling `write(frame_index, guitar,
    /// click)` for each. Pulls input from `input_rx` (silence if it runs
    /// dry), applies the input gain, runs the chain on whole blocks, and
    /// forwards the processed guitar (without the click, which would fool
    /// the tuner and practice mode) to the visualizer.
    fn render(
        &mut self,
        frames: usize,
        pedal_rx: &Receiver<Vec<PedalInstance>>,
        input_rx: &Receiver<i16>,
        audio_tx: &Sender<i16>,
        mut write: impl FnMut(usize, i16, i16),
    ) {
        for msg in pedal_rx.try_iter() {
            self.pedal_chain.apply_description(msg);
        }

        let queued = input_rx.len();
        INPUT_BACKLOG_SAMPLES.store(queued as u32, Ordering::Relaxed);
        for _ in 0..self.trimmer.samples_to_drop(queued, frames) {
            let _ = input_rx.try_recv();
        }

        let input_gain = self.controls.input_gain();
        let unity = (input_gain - 1.0).abs() < 1e-6;
        let mut done = 0;
        while done < frames {
            let len = (frames - done).min(self.block.len());
            let block = &mut self.block[..len];
            let mut peak: i16 = 0;
            for sample in block.iter_mut() {
                let raw = input_rx.try_recv().unwrap_or(0);
                *sample = if unity {
                    raw
                } else {
                    (raw as f32 * input_gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16
                };
                // What reaches the chain, but a clipping interface still
                // shows even with the gain turned down.
                let raw_clip = if raw.saturating_abs() == i16::MAX { i16::MAX } else { 0 };
                peak = peak.max(sample.saturating_abs()).max(raw_clip);
            }
            self.controls.record_input_peak(peak as f32 / i16::MAX as f32);
            let dry_tap = self.controls.analysis_dry();
            if dry_tap {
                self.dry[..len].copy_from_slice(block);
            }
            self.pedal_chain.process_block(block);
            if self.controls.take_pulse_request() {
                self.pulse_remaining = PULSE_SAMPLES;
                self.controls.mark_pulse_sent(self.controls.analysis_sent());
            }
            let mut delivered = 0u64;
            for (i, &sample) in block.iter().enumerate() {
                let click = self.metronome_click.next_sample().saturating_add(next_pulse_sample(&mut self.pulse_remaining));
                let analysed = if dry_tap { self.dry[i] } else { sample };
                // Only delivered samples count, so the UI's received count
                // stays aligned with this one (latency probe).
                delivered += u64::from(audio_tx.try_send(analysed).is_ok());
                write(done + i, sample, click);
            }
            self.controls.count_analysis_samples(delivered);
            done += len;
        }
    }
}

/// Everything an output stream needs besides its device: the channels and
/// shared state it is wired to. Cloned into every rebuilt output stream
/// (device or buffer-size changes) so they all share the same state.
#[derive(Clone)]
pub struct OutputShared {
    pub input_rx: Receiver<i16>,
    pub audio_tx: Sender<i16>,
    pub pedal_rx: Receiver<Vec<PedalInstance>>,
    pub last_output_callback_ns: Arc<AtomicU64>,
    pub metronome_running: Arc<AtomicBool>,
    pub metronome_bpm: Arc<AtomicU32>,
    pub song_player: SongPlayer,
    pub controls: Arc<AudioControls>,
    /// Recorded samples, while `controls.record_source()` is set.
    pub record_tx: Sender<f32>,
}

/// Mixes one output frame: guitar + click + song, through the master
/// volume. Returns `(speaker sample, sample to record)` in -1..1.
#[inline]
fn mix_frame(guitar: i16, click: i16, song: f32, volume: f32, source: Option<RecordSource>) -> (f32, f32) {
    let guitar = guitar as f32 / i16::MAX as f32;
    let full = guitar + click as f32 / i16::MAX as f32 + song;
    let out = (full * volume).clamp(-1.0, 1.0);
    let recorded = match source {
        Some(RecordSource::Guitar) => guitar,
        _ => full.clamp(-1.0, 1.0),
    };
    (out, recorded)
}

/// Length of the metronome's click sound, in samples at `GLOBAL_CONFIG.sample_rate`.
const METRONOME_CLICK_SAMPLES: u32 = 1440; // ~30ms at 48kHz
/// Tone frequency of the metronome click, in Hz.
const METRONOME_CLICK_FREQ_HZ: f32 = 1500.0;
/// Peak amplitude of the metronome click, as a fraction of `i16::MAX` (kept
/// well below the input signal's headroom so it doesn't dominate the mix).
const METRONOME_CLICK_GAIN: f32 = 0.5;

/// Sample-accurate click generator for the metronome, driven by realtime state
/// (`running`/`bpm`) shared with the UI thread via atomics. Runs entirely on the
/// audio callback thread so beat timing isn't subject to UI frame-rate jitter.
struct MetronomeClick {
    running: Arc<AtomicBool>,
    bpm_bits: Arc<AtomicU32>,
    controls: Arc<AudioControls>,
    samples_into_beat: u64,
    beat_in_bar: u32,
    click_remaining: u32,
    click_phase: f32,
    click_freq: f32,
    click_gain: f32,
}

/// Pitch of the accented first beat of each bar.
const METRONOME_ACCENT_FREQ_HZ: f32 = 2200.0;

impl MetronomeClick {
    fn new(running: Arc<AtomicBool>, bpm_bits: Arc<AtomicU32>, controls: Arc<AudioControls>) -> Self {
        Self {
            running,
            bpm_bits,
            controls,
            samples_into_beat: 0,
            beat_in_bar: 0,
            click_remaining: 0,
            click_phase: 0.0,
            click_freq: METRONOME_CLICK_FREQ_HZ,
            click_gain: METRONOME_CLICK_GAIN,
        }
    }

    /// Advances the beat clock by one sample and returns the click contribution
    /// (silence outside of a click envelope, or when stopped) to mix in.
    fn next_sample(&mut self) -> i16 {
        if self.running.load(Ordering::Relaxed) {
            let bpm = f32::from_bits(self.bpm_bits.load(Ordering::Relaxed)).clamp(1.0, 1000.0);
            let period_samples = ((GLOBAL_CONFIG.sample_rate * 60.0 / bpm) as u64).max(1);
            if self.samples_into_beat == 0 {
                let beats_per_bar = self.controls.metronome_beats_per_bar();
                if beats_per_bar == 0 || self.beat_in_bar >= beats_per_bar {
                    self.beat_in_bar = 0;
                }
                let accent = beats_per_bar > 1 && self.beat_in_bar == 0;
                let volume = self.controls.metronome_volume();
                self.click_freq = if accent { METRONOME_ACCENT_FREQ_HZ } else { METRONOME_CLICK_FREQ_HZ };
                self.click_gain = METRONOME_CLICK_GAIN * volume * if accent { 1.0 } else { 0.7 };
                self.click_remaining = METRONOME_CLICK_SAMPLES;
                self.click_phase = 0.0;
                self.controls.publish_metronome_click(self.beat_in_bar);
                self.beat_in_bar += 1;
            }
            self.samples_into_beat = (self.samples_into_beat + 1) % period_samples;
        } else {
            self.samples_into_beat = 0;
            self.beat_in_bar = 0;
            self.click_remaining = 0;
        }

        if self.click_remaining > 0 {
            let envelope = self.click_remaining as f32 / METRONOME_CLICK_SAMPLES as f32;
            let value = (self.click_phase * 2.0 * std::f32::consts::PI * self.click_freq
                / GLOBAL_CONFIG.sample_rate)
                .sin()
                * envelope
                * envelope
                * i16::MAX as f32
                * self.click_gain.min(1.0);
            self.click_phase += 1.0;
            self.click_remaining -= 1;
            value as i16
        } else {
            0
        }
    }
}

/// Mono capture for a sample type without a hand-written path: takes the
/// first channel of each frame and converts it to `i16` through `f32`.
fn build_typed_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: u16,
    input_tx: Sender<i16>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            for frame in data.chunks(channels.max(1) as usize) {
                if let Some(&sample) = frame.first() {
                    let value = <f32 as cpal::FromSample<T>>::from_sample_(sample);
                    let sample_i16 = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    // Non-blocking, like the I16/F32 paths (see module docs).
                    let _ = input_tx.try_send(sample_i16);
                }
            }
        },
        |err| eprintln!("Erro no input stream: {}", err),
        None,
    )
}

/// Builds a mono input stream for sample formats other than I16/F32 (which
/// have their own paths), e.g. the 24-bit integer default of many ALSA
/// hardware devices.
fn build_converted_input_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    channels: u16,
    input_tx: Sender<i16>,
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    use cpal::SampleFormat as F;
    let stream = match format {
        F::I8 => build_typed_input_stream::<i8>(device, config, channels, input_tx)?,
        F::I16 => build_typed_input_stream::<i16>(device, config, channels, input_tx)?,
        F::I24 => build_typed_input_stream::<cpal::I24>(device, config, channels, input_tx)?,
        F::I32 => build_typed_input_stream::<i32>(device, config, channels, input_tx)?,
        F::I64 => build_typed_input_stream::<i64>(device, config, channels, input_tx)?,
        F::U8 => build_typed_input_stream::<u8>(device, config, channels, input_tx)?,
        F::U16 => build_typed_input_stream::<u16>(device, config, channels, input_tx)?,
        F::U24 => build_typed_input_stream::<cpal::U24>(device, config, channels, input_tx)?,
        F::U32 => build_typed_input_stream::<u32>(device, config, channels, input_tx)?,
        F::U64 => build_typed_input_stream::<u64>(device, config, channels, input_tx)?,
        F::F32 => build_typed_input_stream::<f32>(device, config, channels, input_tx)?,
        F::F64 => build_typed_input_stream::<f64>(device, config, channels, input_tx)?,
        other => {
            return Err(crate::trf!("Unsupported input sample format: {other:?}", "Formato de amostra de entrada não suportado: {other:?}").into());
        }
    };
    Ok(stream)
}

/// Turns a device's `SupportedStreamConfig` into a concrete `StreamConfig`,
/// overriding the buffer size to `requested_frames` when the device reports a
/// queryable supported range (clamping into that range), or leaving it at
/// `BufferSize::Default` when the range is unknown (some hosts/devices can't
/// report one, in which case requesting an arbitrary fixed size risks a
/// stream-build error rather than silently doing the right thing).
///
/// Returns the concrete config plus the buffer size actually applied, in
/// frames (`None` when left at the host default because the range was
/// unknown, since the real value in that case isn't known until playback).
pub fn resolve_stream_config(
    supported: &cpal::SupportedStreamConfig,
    requested_frames: u32,
) -> (cpal::StreamConfig, Option<u32>) {
    let mut config = supported.config();
    match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            let clamped = requested_frames.clamp(*min, *max);
            config.buffer_size = cpal::BufferSize::Fixed(clamped);
            (config, Some(clamped))
        }
        cpal::SupportedBufferSize::Unknown => (config, None),
    }
}

/// Extracts up to `channel_count` samples from one interleaved device frame
/// (`frame[0]` = channel 0, `frame[1]` = channel 1, ...). Shared by the legacy
/// single-channel path (which historically just took `frame.first()`, equivalent
/// to `extract_channels(frame, 1)`) and the newer multi-channel path.
fn extract_channels<T: Copy>(frame: &[T], channel_count: usize) -> Vec<T> {
    frame.iter().take(channel_count).copied().collect()
}

pub struct AudioSetup {
    /// `None` when the app runs without audio (see [`AudioSetup::without_audio`]).
    pub input_stream: Option<cpal::Stream>,
    pub output_stream: Option<cpal::Stream>,
    pub input_tx: Sender<i16>,
    pub pedal_tx: Sender<Vec<PedalInstance>>,
    // Kept around so the output stream can be rebuilt on a different device later
    // (see `switch_output_device`) with the same processing pipeline.
    pub input_rx: Receiver<i16>,
    pub pedal_rx: Receiver<Vec<PedalInstance>>,
    pub audio_tx: Sender<i16>,
    /// Nominal round-trip buffer latency in seconds (`buffer_size / sample_rate` from
    /// `GLOBAL_CONFIG`), computed once at setup time. See module docs for context.
    pub configured_latency_secs: f32,
    /// Wall-clock duration of the most recently completed output-stream callback, in
    /// nanoseconds. Updated from the realtime audio thread on every callback; safe to read
    /// from any thread via `.load(Ordering::Relaxed)`. See module docs for context.
    pub last_output_callback_ns: Arc<AtomicU64>,
    /// Whether the metronome click should currently be audible. Read from the
    /// realtime output callback; written from the UI when the user starts/stops
    /// the metronome (see `frontend::ui::metronome::MetronomePanel`).
    pub metronome_running: Arc<AtomicBool>,
    /// Current metronome tempo, stored as `f32::to_bits` since `AtomicF32`
    /// doesn't exist in `std`.
    pub metronome_bpm: Arc<AtomicU32>,
    /// Tab-player song engine (synth + backing track), mixed into the output
    /// after the pedal chain. See `backend::song_player`.
    pub song_player: SongPlayer,
    /// Master volume, mute, input gain, metronome accent and level meters.
    pub controls: Arc<AudioControls>,
    pub record_tx: Sender<f32>,
    /// Read by `backend::recorder::Recorder`.
    pub record_rx: Receiver<f32>,
}

impl AudioSetup {
    /// The channels and shared state, with no streams: the app still opens
    /// (and can retry the devices later) when no audio device works.
    pub fn without_audio(audio_tx: Sender<i16>) -> Self {
        let (input_tx, input_rx) = bounded::<i16>(4096);
        let (pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(8);
        let (record_tx, record_rx) = record_channel();
        Self {
            input_stream: None,
            output_stream: None,
            input_tx,
            pedal_tx,
            input_rx,
            pedal_rx,
            audio_tx,
            configured_latency_secs: 2.0 * GLOBAL_CONFIG.buffer_size as f32 / GLOBAL_CONFIG.sample_rate,
            last_output_callback_ns: Arc::new(AtomicU64::new(0)),
            metronome_running: Arc::new(AtomicBool::new(false)),
            metronome_bpm: Arc::new(AtomicU32::new(120.0f32.to_bits())),
            song_player: SongPlayer::new(),
            controls: Arc::new(AudioControls::default()),
            record_tx,
            record_rx,
        }
    }

    /// What `switch_output_device` needs to rebuild the output stream.
    pub fn output_shared(&self) -> OutputShared {
        OutputShared {
            input_rx: self.input_rx.clone(),
            audio_tx: self.audio_tx.clone(),
            pedal_rx: self.pedal_rx.clone(),
            last_output_callback_ns: self.last_output_callback_ns.clone(),
            metronome_running: self.metronome_running.clone(),
            metronome_bpm: self.metronome_bpm.clone(),
            song_player: self.song_player.clone(),
            controls: self.controls.clone(),
            record_tx: self.record_tx.clone(),
        }
    }
}

pub fn start_audio_processing(
    audio_tx: Sender<i16>,
) -> Result<AudioSetup, Box<dyn std::error::Error>> {
    start_audio_processing_with_devices(audio_tx, None, None, GLOBAL_CONFIG.buffer_size as u32)
}

/// Like `start_audio_processing`, but tries to open `preferred_input_name` /
/// `preferred_output_name` (matched by exact device name) instead of the
/// host's default device, falling back to the default if no device with that
/// name is currently available, and requests `buffer_frames` as the host
/// callback buffer size (clamped to what the device actually supports; see
/// `resolve_stream_config`). Used to restore the last-used devices/buffer
/// size from `shared::settings::AppSettings` (Phase 3.4) without duplicating
/// the device-switching machinery used for live switches in `frontend::gui`.
pub fn start_audio_processing_with_devices(
    audio_tx: Sender<i16>,
    preferred_input_name: Option<&str>,
    preferred_output_name: Option<&str>,
    buffer_frames: u32,
) -> Result<AudioSetup, Box<dyn std::error::Error>> {
    let host = cpal::default_host();

    let input_device = preferred_input_name
        .and_then(|name| {
            host.input_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.to_string() == name))
        })
        .or_else(|| host.default_input_device())
        .ok_or(crate::shared::i18n::tr("No audio input device found", "Nenhum dispositivo de entrada de áudio encontrado"))?;
    let output_device = preferred_output_name
        .and_then(|name| {
            host.output_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.to_string() == name))
        })
        .or_else(|| host.default_output_device())
        .ok_or(crate::shared::i18n::tr("No audio output device found", "Nenhum dispositivo de saída de áudio encontrado"))?;

    println!("Dispositivo de Entrada Inicial: {:?}", input_device.to_string());
    println!("Dispositivo de Saída: {:?}", output_device.to_string());

    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;

    let input_config = find_best_config(&input_device, target_sample_rate, true)?;
    let output_config = find_best_config(&output_device, target_sample_rate, false)?;

    println!("Configuração de Entrada Utilizada: {:?}", input_config);
    println!("Configuração de Saída Utilizada: {:?}", output_config);

    // Canal para transferir áudio da captura (input) para a reprodução (output)
    let (input_tx, input_rx) = bounded::<i16>(4096);

    // Construção inicial do Stream de Captura (Input)
    let input_sample_format = input_config.sample_format();
    let (input_stream_config, input_effective_frames) =
        resolve_stream_config(&input_config, buffer_frames);
    let input_channels = input_stream_config.channels;
    let input_tx_clone = input_tx.clone();

    let input_stream = match input_sample_format {
        cpal::SampleFormat::I16 => input_device.build_input_stream(
            input_stream_config,
            move |data: &[i16], _| {
                for frame in data.chunks(input_channels as usize) {
                    if let Some(&sample) = frame.first() {
                        // Non-blocking: the capture callback must never park waiting for the
                        // output side to drain (see module docs). Drop the sample instead.
                        let _ = input_tx_clone.try_send(sample);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            input_stream_config,
            move |data: &[f32], _| {
                for frame in data.chunks(input_channels as usize) {
                    if let Some(&sample) = frame.first() {
                        let sample_i16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        // Non-blocking: see module docs — dropping is preferable to blocking
                        // the realtime capture callback.
                        let _ = input_tx_clone.try_send(sample_i16);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (F32): {}", err),
            None
        )?,
        other => build_converted_input_stream(
            &input_device,
            input_stream_config,
            other,
            input_channels,
            input_tx_clone,
        )?,
    };

    input_stream.play()?;

    // Canal para passar atualizações de pedais para a thread de reprodução
    let (pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(8);

    let last_output_callback_ns = Arc::new(AtomicU64::new(0));
    let metronome_running = Arc::new(AtomicBool::new(false));
    let metronome_bpm = Arc::new(AtomicU32::new(120.0f32.to_bits()));
    let song_player = SongPlayer::new();
    let controls = Arc::new(AudioControls::default());
    let (record_tx, record_rx) = record_channel();

    let shared = OutputShared {
        input_rx: input_rx.clone(),
        audio_tx: audio_tx.clone(),
        pedal_rx: pedal_rx.clone(),
        last_output_callback_ns: last_output_callback_ns.clone(),
        metronome_running: metronome_running.clone(),
        metronome_bpm: metronome_bpm.clone(),
        song_player: song_player.clone(),
        controls: controls.clone(),
        record_tx: record_tx.clone(),
    };
    let (output_stream, output_effective_frames) =
        build_output_stream(&output_device, output_config, buffer_frames, shared, Vec::new())?;

    output_stream.play()?;

    // Nominal round-trip latency: input-side + output-side buffer, each converted
    // to seconds. Falls back to the requested (unclamped) frame count for whichever
    // side couldn't report a supported range (see `resolve_stream_config`), which
    // matches the previous fixed-`GLOBAL_CONFIG.buffer_size` estimate's precision.
    let configured_latency_secs = (input_effective_frames.unwrap_or(buffer_frames)
        + output_effective_frames.unwrap_or(buffer_frames)) as f32
        / GLOBAL_CONFIG.sample_rate;

    Ok(AudioSetup {
        input_stream: Some(input_stream),
        output_stream: Some(output_stream),
        input_tx,
        pedal_tx,
        input_rx,
        pedal_rx,
        audio_tx,
        configured_latency_secs,
        last_output_callback_ns,
        metronome_running,
        metronome_bpm,
        song_player,
        controls,
        record_tx,
        record_rx,
    })
}

/// Builds the output stream: drains pedal-chain updates, pulls processed samples off
/// `input_rx`, mixes in the metronome and the tab-player song through the master
/// volume, and forwards the guitar to `audio_tx` for visualization. Returns the
/// stream plus the buffer size actually applied (see `resolve_stream_config`).
fn build_output_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    buffer_frames: u32,
    shared: OutputShared,
    initial_pedals: Vec<PedalInstance>,
) -> Result<(cpal::Stream, Option<u32>), Box<dyn std::error::Error>> {
    let sample_format = config.sample_format();
    let (stream_config, effective_frames) = resolve_stream_config(&config, buffer_frames);
    let channels = stream_config.channels;
    let OutputShared {
        input_rx,
        audio_tx,
        pedal_rx,
        last_output_callback_ns,
        metronome_running,
        metronome_bpm,
        song_player,
        controls,
        record_tx,
    } = shared;
    song_player.set_output_rate(stream_config.sample_rate);
    // Song audio for the current callback, rendered in one block before the
    // per-frame loop. Sized generously up front so the realtime callback
    // only reallocates if the host hands over an unusually large buffer.
    let mut song_buffer: Vec<f32> = vec![0.0; 8192];

    let pedal_chain = PedalChain::from_description(initial_pedals);
    let metronome_click = MetronomeClick::new(metronome_running, metronome_bpm, controls.clone());
    let mut renderer = OutputRenderer::new(pedal_chain, metronome_click, controls.clone());
    let channels_usize = (channels as usize).max(1);

    // One callback body for both sample formats: `to_sample` converts the
    // mixed -1..1 value.
    macro_rules! callback {
        ($ty:ty, $to_sample:expr) => {
            move |data: &mut [$ty], _: &cpal::OutputCallbackInfo| {
                let callback_start = Instant::now();
                let frames = data.len() / channels_usize;
                let song = render_song_block(&song_player, &mut song_buffer, frames);
                let volume = controls.effective_volume();
                let record = controls.record_source();
                let mut peak = 0.0f32;
                renderer.render(frames, &pedal_rx, &input_rx, &audio_tx, |i, guitar, click| {
                    let (out, recorded) = mix_frame(guitar, click, song[i], volume, record);
                    peak = peak.max(out.abs());
                    if record.is_some() {
                        let _ = record_tx.try_send(recorded);
                    }
                    let start = i * channels_usize;
                    data[start..start + channels_usize].fill($to_sample(out));
                });
                controls.record_output_peak(peak);
                last_output_callback_ns.store(callback_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
            }
        };
    }

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            stream_config,
            callback!(i16, |v: f32| (v * i16::MAX as f32) as i16),
            |err| eprintln!("Erro no output stream (I16): {}", err),
            None,
        )?,
        cpal::SampleFormat::F32 => device.build_output_stream(
            stream_config,
            callback!(f32, |v: f32| v),
            |err| eprintln!("Erro no output stream (F32): {}", err),
            None,
        )?,
        _ => return Err(crate::shared::i18n::tr("Output sample format not supported by CPAL", "Formato de amostra de saída não suportado pelo CPAL").into()),
    };

    Ok((stream, effective_frames))
}

/// The output stream's processing without a device, run on demand: for
/// offscreen tools (`tools/promo_video`) that drive an
/// [`AudioSetup::without_audio`] app with recorded guitar. Push samples into
/// the setup's `input_tx`, call [`OfflineOutput::render`], and the app sees
/// what a live stream would give it: chain updates applied, meters, the
/// analysis tap, metronome clicks, tab playback and recording.
pub struct OfflineOutput {
    renderer: OutputRenderer,
    shared: OutputShared,
    song_buffer: Vec<f32>,
    out: Vec<f32>,
}

impl OfflineOutput {
    pub fn new(setup: &AudioSetup, initial_pedals: Vec<PedalInstance>) -> Self {
        let shared = setup.output_shared();
        shared.song_player.set_output_rate(GLOBAL_CONFIG.sample_rate as u32);
        let metronome_click =
            MetronomeClick::new(shared.metronome_running.clone(), shared.metronome_bpm.clone(), shared.controls.clone());
        let renderer =
            OutputRenderer::new(PedalChain::from_description(initial_pedals), metronome_click, shared.controls.clone());
        Self { renderer, shared, song_buffer: Vec::new(), out: Vec::new() }
    }

    /// Renders `frames` mono samples of what the speakers get (-1..1), as
    /// one output callback would.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        let OutputShared { input_rx, audio_tx, pedal_rx, song_player, controls, record_tx, .. } = &self.shared;
        let song = render_song_block(song_player, &mut self.song_buffer, frames);
        let volume = controls.effective_volume();
        let record = controls.record_source();
        self.out.clear();
        self.out.resize(frames, 0.0);
        let out = &mut self.out;
        let mut peak = 0.0f32;
        self.renderer.render(frames, pedal_rx, input_rx, audio_tx, |i, guitar, click| {
            let (sample, recorded) = mix_frame(guitar, click, song[i], volume, record);
            peak = peak.max(sample.abs());
            if record.is_some() {
                let _ = record_tx.try_send(recorded);
            }
            out[i] = sample;
        });
        controls.record_output_peak(peak);
        &self.out
    }
}

/// Renders `frames` frames of tab-player audio into `buffer` (growing it if
/// the host asks for an unusually large callback) and returns that slice.
/// Never blocks: see `SongPlayer::try_render`.
fn render_song_block<'a>(player: &SongPlayer, buffer: &'a mut Vec<f32>, frames: usize) -> &'a [f32] {
    if buffer.len() < frames {
        buffer.resize(frames, 0.0);
    }
    let block = &mut buffer[..frames];
    block.fill(0.0);
    player.try_render(block);
    block
}

/// Finds the named output device, builds a fresh output stream for it (seeded with
/// `initial_pedals` so the pedal chain doesn't reset to empty), requesting
/// `buffer_frames` as the callback buffer size, and starts it playing. The new
/// stream shares everything in `shared` with the one it replaces, including the
/// callback timing the UI's CPU meter reads.
pub fn switch_output_device(
    device_name: &str,
    buffer_frames: u32,
    shared: OutputShared,
    initial_pedals: Vec<PedalInstance>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let devices = host.output_devices().map_err(|e| e.to_string())?;
    let device = devices
        .into_iter()
        .find(|d| d.to_string() == device_name)
        .or_else(|| device_name.is_empty().then(|| host.default_output_device()).flatten())
        .ok_or_else(|| crate::shared::i18n::tr("Output device not found", "Dispositivo de saída não encontrado").to_string())?;

    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;
    let config = find_best_config(&device, target_sample_rate, false).map_err(|e| e.to_string())?;

    let (stream, _effective_frames) =
        build_output_stream(&device, config, buffer_frames, shared, initial_pedals).map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}


/// Finds the named input device, builds a fresh input stream for it requesting
/// `buffer_frames` as the callback buffer size, and starts it playing. Mirrors
/// `switch_output_device`; factored out of `frontend::gui`'s inline device-switch
/// closure so live device switches and buffer-size changes share one
/// implementation instead of two copies of the same cpal setup dance.
pub fn switch_input_device(
    device_name: &str,
    buffer_frames: u32,
    input_tx: Sender<i16>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|e| e.to_string())?;
    let device = devices
        .into_iter()
        .find(|d| d.to_string() == device_name)
        .or_else(|| device_name.is_empty().then(|| host.default_input_device()).flatten())
        .ok_or_else(|| crate::shared::i18n::tr("Input device not found", "Dispositivo de entrada não encontrado").to_string())?;

    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;
    let config = find_best_config(&device, target_sample_rate, true).map_err(|e| e.to_string())?;
    let sample_format = config.sample_format();
    let (stream_config, _effective_frames) = resolve_stream_config(&config, buffer_frames);
    let channels = stream_config.channels;

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                stream_config,
                move |data: &[i16], _| {
                    for frame in data.chunks(channels as usize) {
                        if let Some(&sample) = frame.first() {
                            let _ = input_tx.try_send(sample);
                        }
                    }
                },
                |err| eprintln!("Erro no input stream (I16): {}", err),
                None,
            )
            .map_err(|e| e.to_string())?,
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                stream_config,
                move |data: &[f32], _| {
                    for frame in data.chunks(channels as usize) {
                        if let Some(&sample) = frame.first() {
                            let sample_i16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            let _ = input_tx.try_send(sample_i16);
                        }
                    }
                },
                |err| eprintln!("Erro no input stream (F32): {}", err),
                None,
            )
            .map_err(|e| e.to_string())?,
        other => build_converted_input_stream(&device, stream_config, other, channels, input_tx)
            .map_err(|e| e.to_string())?,
    };

    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

pub fn find_best_config(
    device: &cpal::Device,
    _target_sample_rate: u32,
    is_input: bool,
) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
    let default_config = if is_input {
        device.default_input_config()?
    } else {
        device.default_output_config()?
    };
    Ok(default_config)
}

/// Additive, opt-in multi-channel variant of [`AudioSetup`]. `start_audio_processing`
/// (the default/legacy entry point used by both `guitar_desktop` binaries and the UI)
/// is untouched and keeps its single mono `Sender<i16>`/`Receiver<i16>` pipeline;
/// this struct instead carries per-frame vectors (one `i16` per captured channel) so
/// callers that want independent per-channel processing can opt into it without any
/// change to the existing mono contract.
pub struct MultiChannelAudioSetup {
    pub input_stream: cpal::Stream,
    pub output_stream: cpal::Stream,
    pub input_tx: Sender<Vec<i16>>,
    pub pedal_tx: Sender<Vec<PedalInstance>>,
    pub input_rx: Receiver<Vec<i16>>,
    pub pedal_rx: Receiver<Vec<PedalInstance>>,
    pub audio_tx: Sender<i16>,
    pub channel_count: usize,
}

/// Like `start_audio_processing`, but captures and plays back `channel_count`
/// independent channels instead of always downmixing to a single mono channel.
/// Each channel is routed through its own `PedalChain` instance (via
/// `MultiChannelPedalChain`), so channels never share pedal state (delay lines,
/// filter memory, etc.). Passing `channel_count == 1` reproduces the exact
/// behavior of the legacy mono path.
pub fn start_audio_processing_multi(
    audio_tx: Sender<i16>,
    channel_count: usize,
) -> Result<MultiChannelAudioSetup, Box<dyn std::error::Error>> {
    let channel_count = channel_count.max(1);
    let host = cpal::default_host();

    let input_device = host.default_input_device()
        .ok_or(crate::shared::i18n::tr("No audio input device found", "Nenhum dispositivo de entrada de áudio encontrado"))?;
    let output_device = host.default_output_device()
        .ok_or(crate::shared::i18n::tr("No audio output device found", "Nenhum dispositivo de saída de áudio encontrado"))?;

    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;

    let input_config = find_best_config(&input_device, target_sample_rate, true)?;
    let output_config = find_best_config(&output_device, target_sample_rate, false)?;

    // Canal para transferir os frames (um i16 por canal capturado) da captura para a reprodução
    let (input_tx, input_rx) = bounded::<Vec<i16>>(4096);

    let input_sample_format = input_config.sample_format();
    let input_stream_config = input_config.config();
    let input_channels = input_stream_config.channels;
    let input_tx_clone = input_tx.clone();

    let input_stream = match input_sample_format {
        cpal::SampleFormat::I16 => input_device.build_input_stream(
            input_stream_config,
            move |data: &[i16], _| {
                for frame in data.chunks(input_channels as usize) {
                    let samples = extract_channels(frame, channel_count);
                    if !samples.is_empty() {
                        let _ = input_tx_clone.try_send(samples);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            input_stream_config,
            move |data: &[f32], _| {
                for frame in data.chunks(input_channels as usize) {
                    let samples: Vec<i16> = extract_channels(frame, channel_count)
                        .into_iter()
                        .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    if !samples.is_empty() {
                        let _ = input_tx_clone.try_send(samples);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (F32): {}", err),
            None
        )?,
        _ => return Err(crate::shared::i18n::tr("Input sample format not supported by CPAL", "Formato de amostra de entrada não suportado pelo CPAL").into()),
    };

    input_stream.play()?;

    let (pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(8);

    let output_stream = build_multi_channel_output_stream(
        &output_device,
        output_config,
        input_rx.clone(),
        audio_tx.clone(),
        pedal_rx.clone(),
        Vec::new(),
        channel_count,
    )?;

    output_stream.play()?;

    Ok(MultiChannelAudioSetup {
        input_stream,
        output_stream,
        input_tx,
        pedal_tx,
        input_rx,
        pedal_rx,
        audio_tx,
        channel_count,
    })
}

/// Multi-channel counterpart of `build_output_stream`: runs each captured channel
/// through its own `PedalChain` (via `MultiChannelPedalChain`) and writes each
/// channel's processed sample to the matching output channel. If the output device
/// has more channels than were captured, the extra channels are written with 0
/// (silence), matching the old behavior for devices with more output channels than
/// input channels. Only channel 0's processed sample is forwarded to `audio_tx`,
/// since that channel feeds the (mono) UI oscilloscope/FFT view.
fn build_multi_channel_output_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    input_rx: Receiver<Vec<i16>>,
    audio_tx: Sender<i16>,
    pedal_rx: Receiver<Vec<PedalInstance>>,
    initial_pedals: Vec<PedalInstance>,
    channel_count: usize,
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let sample_format = config.sample_format();
    let stream_config = config.config();
    let output_channels = stream_config.channels;

    let mut pedal_chain = MultiChannelPedalChain::from_description(initial_pedals, channel_count);

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            stream_config,
            move |data: &mut [i16], _| {
                for msg in pedal_rx.try_iter() {
                    pedal_chain = MultiChannelPedalChain::from_description(msg, channel_count);
                }

                for frame in data.chunks_mut(output_channels as usize) {
                    let processed = if let Ok(in_samples) = input_rx.try_recv() {
                        let out_samples = pedal_chain.process_frame(&in_samples);
                        if let Some(&first) = out_samples.first() {
                            let _ = audio_tx.try_send(first);
                        }
                        out_samples
                    } else {
                        Vec::new()
                    };

                    for (idx, channel_sample) in frame.iter_mut().enumerate() {
                        *channel_sample = processed.get(idx).copied().unwrap_or(0);
                    }
                }
            },
            |err| eprintln!("Erro no output stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => device.build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                for msg in pedal_rx.try_iter() {
                    pedal_chain = MultiChannelPedalChain::from_description(msg, channel_count);
                }

                for frame in data.chunks_mut(output_channels as usize) {
                    let processed = if let Ok(in_samples) = input_rx.try_recv() {
                        let out_samples = pedal_chain.process_frame(&in_samples);
                        if let Some(&first) = out_samples.first() {
                            let _ = audio_tx.try_send(first);
                        }
                        out_samples
                    } else {
                        Vec::new()
                    };

                    for (idx, channel_sample) in frame.iter_mut().enumerate() {
                        let sample_i16 = processed.get(idx).copied().unwrap_or(0);
                        *channel_sample = sample_i16 as f32 / i16::MAX as f32;
                    }
                }
            },
            |err| eprintln!("Erro no output stream (F32): {}", err),
            None
        )?,
        _ => return Err(crate::shared::i18n::tr("Output sample format not supported by CPAL", "Formato de amostra de saída não suportado pelo CPAL").into()),
    };

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::pedals::pedal::MultiChannelPedalChain;
    use crate::shared::pedals::{LowPassParams, PedalDescription, PedalInstance};

    fn supported_config(buffer_size: cpal::SupportedBufferSize) -> cpal::SupportedStreamConfig {
        cpal::SupportedStreamConfig::new(2, 48000, buffer_size, cpal::SampleFormat::F32)
    }

    #[test]
    fn trimmer_keeps_a_lean_queue_untouched() {
        let mut trimmer = BacklogTrimmer::default();
        let mut dropped = 0;
        // Queue oscillates around one callback's worth: nothing is surplus.
        for i in 0..1000 {
            dropped += trimmer.samples_to_drop(if i % 2 == 0 { 128 } else { 200 }, 128);
        }
        assert_eq!(dropped, 0);
    }

    #[test]
    fn trimmer_drops_backlog_that_never_drains() {
        let mut trimmer = BacklogTrimmer::default();
        let mut queued = 128 + 2000; // e.g. a stall left 2000 extra samples
        let mut dropped_total = 0;
        for _ in 0..(BACKLOG_WINDOW_SAMPLES / 128 + 1) {
            let dropped = trimmer.samples_to_drop(queued, 128);
            queued -= dropped;
            dropped_total += dropped;
        }
        assert_eq!(dropped_total, 2000 - BACKLOG_MARGIN_SAMPLES);
        assert_eq!(queued, 128 + BACKLOG_MARGIN_SAMPLES);
    }

    #[test]
    fn renderer_passes_input_through_and_pads_silence() {
        let (input_tx, input_rx) = bounded::<i16>(64);
        let (_pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(1);
        let (audio_tx, _audio_rx) = bounded::<i16>(64);
        let controls = Arc::new(AudioControls::default());
        let metronome = MetronomeClick::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(120.0f32.to_bits())),
            controls.clone(),
        );
        let mut renderer = OutputRenderer::new(PedalChain::new(), metronome, controls.clone());
        for s in [10, 20, 30] {
            input_tx.send(s).unwrap();
        }
        let mut out = [99i16; 5];
        renderer.render(5, &pedal_rx, &input_rx, &audio_tx, |i, s, click| {
            assert_eq!(click, 0);
            out[i] = s;
        });
        assert_eq!(out, [10, 20, 30, 0, 0]);
        assert!((controls.take_input_peak() - 30.0 / i16::MAX as f32).abs() < 1e-6);
    }

    #[test]
    fn offline_output_runs_the_stream_pipeline() {
        let (audio_tx, audio_rx) = bounded::<i16>(64);
        let setup = AudioSetup::without_audio(audio_tx);
        let mut output = OfflineOutput::new(&setup, Vec::new());
        for s in [1000i16, -2000, 3000] {
            setup.input_tx.send(s).unwrap();
        }
        let out = output.render(4).to_vec();
        let expected = [1000.0, -2000.0, 3000.0, 0.0].map(|s: f32| s / i16::MAX as f32);
        for (o, e) in out.iter().zip(expected) {
            assert!((o - e).abs() < 1e-6, "{out:?}");
        }
        assert_eq!(audio_rx.try_iter().collect::<Vec<_>>(), [1000, -2000, 3000, 0]);
        assert!((setup.controls.take_input_peak() - 3000.0 / i16::MAX as f32).abs() < 1e-6);
        assert!((setup.controls.take_output_peak() - 3000.0 / i16::MAX as f32).abs() < 1e-6);
    }

    #[test]
    fn input_gain_scales_the_guitar_before_the_chain() {
        let (input_tx, input_rx) = bounded::<i16>(64);
        let (_pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(1);
        let (audio_tx, audio_rx) = bounded::<i16>(64);
        let controls = Arc::new(AudioControls::default());
        controls.set_input_gain(2.0);
        let metronome =
            MetronomeClick::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)), controls.clone());
        let mut renderer = OutputRenderer::new(PedalChain::new(), metronome, controls.clone());
        for s in [100, -20_000] {
            input_tx.send(s).unwrap();
        }
        let mut out = [0i16; 2];
        renderer.render(2, &pedal_rx, &input_rx, &audio_tx, |i, s, _| out[i] = s);
        assert_eq!(out, [200, i16::MIN]);
        // The visualizer gets the same (gained) guitar signal.
        assert_eq!(audio_rx.try_iter().collect::<Vec<_>>(), vec![200, i16::MIN]);
    }

    #[test]
    fn dry_tap_sends_the_input_and_pulses_are_placed_in_the_analysis_stream() {
        use crate::shared::pedals::{AmpParams, PedalDescription};
        let (input_tx, input_rx) = bounded::<i16>(64);
        let (pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(1);
        let (audio_tx, audio_rx) = bounded::<i16>(64);
        let controls = Arc::new(AudioControls::default());
        let metronome =
            MetronomeClick::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)), controls.clone());
        let mut renderer = OutputRenderer::new(PedalChain::new(), metronome, controls.clone());
        pedal_tx.send(vec![PedalInstance::new(PedalDescription::Amp(AmpParams { gain: 4.0, ..Default::default() }))]).unwrap();
        for s in [1000, 2000, 3000, 4000] {
            input_tx.send(s).unwrap();
        }
        let mut clicks = Vec::new();
        renderer.render(2, &pedal_rx, &input_rx, &audio_tx, |_, _, _| {});
        controls.request_pulse();
        renderer.render(2, &pedal_rx, &input_rx, &audio_tx, |_, _, click| clicks.push(click));
        assert_eq!(audio_rx.try_iter().collect::<Vec<_>>(), vec![1000, 2000, 3000, 4000], "dry by default");
        assert_eq!(controls.pulse_sent_at(), Some(2), "pulse starts at the third analysis sample");
        assert_eq!(controls.analysis_sent(), 4);
        assert!(clicks[1] != 0, "pulse is audible");
    }

    #[test]
    fn metronome_accents_the_first_beat_and_reports_clicks() {
        let controls = Arc::new(AudioControls::default());
        controls.set_metronome_beats_per_bar(3);
        let running = Arc::new(AtomicBool::new(true));
        // 960 bpm = one beat per 3000 samples at 48 kHz.
        let mut click = MetronomeClick::new(running, Arc::new(AtomicU32::new(960.0f32.to_bits())), controls.clone());
        let mut beats = Vec::new();
        let mut clicks_seen = 0;
        for _ in 0..9500 {
            click.next_sample();
            let (beat, clicks) = controls.metronome_position();
            if clicks != clicks_seen {
                clicks_seen = clicks;
                beats.push((beat, click.click_freq));
            }
        }
        assert_eq!(
            beats,
            vec![
                (0, METRONOME_ACCENT_FREQ_HZ),
                (1, METRONOME_CLICK_FREQ_HZ),
                (2, METRONOME_CLICK_FREQ_HZ),
                (0, METRONOME_ACCENT_FREQ_HZ)
            ]
        );
    }

    #[test]
    fn mix_applies_volume_to_the_speakers_but_not_the_recording() {
        let half = i16::MAX / 2;
        let (out, recorded) = mix_frame(half, 0, 0.25, 0.5, Some(RecordSource::Mix));
        assert!((out - 0.375).abs() < 1e-3);
        assert!((recorded - 0.75).abs() < 1e-3);
        let (_, guitar_only) = mix_frame(half, half, 0.25, 0.0, Some(RecordSource::Guitar));
        assert!((guitar_only - 0.5).abs() < 1e-3);
    }

    #[test]
    fn resolve_stream_config_clamps_requested_frames_into_supported_range() {
        let supported = supported_config(cpal::SupportedBufferSize::Range { min: 64, max: 1024 });

        let (config, effective) = resolve_stream_config(&supported, 256);
        assert_eq!(effective, Some(256));
        assert_eq!(config.buffer_size, cpal::BufferSize::Fixed(256));

        // Below the device's minimum: clamp up.
        let (config, effective) = resolve_stream_config(&supported, 16);
        assert_eq!(effective, Some(64));
        assert_eq!(config.buffer_size, cpal::BufferSize::Fixed(64));

        // Above the device's maximum: clamp down.
        let (config, effective) = resolve_stream_config(&supported, 8192);
        assert_eq!(effective, Some(1024));
        assert_eq!(config.buffer_size, cpal::BufferSize::Fixed(1024));
    }

    #[test]
    fn resolve_stream_config_leaves_default_when_range_unknown() {
        let supported = supported_config(cpal::SupportedBufferSize::Unknown);

        let (config, effective) = resolve_stream_config(&supported, 256);
        assert_eq!(effective, None);
        assert_eq!(config.buffer_size, cpal::BufferSize::Default);
    }

    #[test]
    fn extract_channels_takes_only_requested_count() {
        let frame = [10i16, 20, 30, 40];
        assert_eq!(extract_channels(&frame, 1), vec![10]);
        assert_eq!(extract_channels(&frame, 2), vec![10, 20]);
        assert_eq!(extract_channels(&frame, 4), vec![10, 20, 30, 40]);
        // Requesting more channels than the frame has just yields what's present.
        assert_eq!(extract_channels(&frame, 10), vec![10, 20, 30, 40]);
    }

    #[test]
    fn extract_channels_with_one_channel_matches_legacy_first_only_behavior() {
        // The old code did `frame.first()`; `extract_channels(frame, 1)` must be
        // equivalent for the default/legacy mono path.
        let frame = [42i16, 99, 7];
        assert_eq!(extract_channels(&frame, 1).first().copied(), frame.first().copied());
    }

    /// End-to-end (minus real cpal hardware) proof that a 2-channel capture, once
    /// demultiplexed by `extract_channels` and routed through
    /// `MultiChannelPedalChain`, keeps both channels' processed output independent
    /// -- i.e. capturing/playing two channels doesn't cross-contaminate them.
    #[test]
    fn two_channel_capture_and_processing_stays_independent() {
        // Simulate raw interleaved frames as cpal would deliver them: (ch0, ch1) pairs.
        let raw_frames: [[i16; 2]; 5] = [
            [15000, 0],
            [-15000, 0],
            [15000, 0],
            [-15000, 0],
            [15000, 0],
        ];

        let description = vec![PedalInstance::new(PedalDescription::LowPass(LowPassParams { frequency: 1000.0 }))];
        let mut chain = MultiChannelPedalChain::from_description(description, 2);

        let mut channel0_playback = Vec::new();
        let mut channel1_playback = Vec::new();

        for raw_frame in raw_frames.iter() {
            let captured = extract_channels(raw_frame, 2);
            let processed = chain.process_frame(&captured);
            channel0_playback.push(processed[0]);
            channel1_playback.push(processed[1]);
        }

        // Channel 0 got a real alternating signal, so the low-pass filter should
        // produce non-zero output somewhere in the sequence.
        assert!(channel0_playback.iter().any(|&s| s != 0));
        // Channel 1 was pure silence throughout capture and playback; it must stay
        // silent -- any nonzero value would mean channel 0 leaked into channel 1.
        assert!(channel1_playback.iter().all(|&s| s == 0));
    }
}
