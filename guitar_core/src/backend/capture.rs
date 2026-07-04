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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use crate::backend::pedals::pedal::PedalChain;
use crate::shared::pedals::PedalDescription;
use crate::shared::config::GLOBAL_CONFIG;

pub struct AudioSetup {
    pub input_stream: cpal::Stream,
    pub output_stream: cpal::Stream,
    pub input_tx: Sender<i16>,
    pub pedal_tx: Sender<Vec<PedalDescription>>,
    // Kept around so the output stream can be rebuilt on a different device later
    // (see `switch_output_device`) with the same processing pipeline.
    pub input_rx: Receiver<i16>,
    pub pedal_rx: Receiver<Vec<PedalDescription>>,
    pub audio_tx: Sender<i16>,
    /// Nominal round-trip buffer latency in seconds (`buffer_size / sample_rate` from
    /// `GLOBAL_CONFIG`), computed once at setup time. See module docs for context.
    pub configured_latency_secs: f32,
    /// Wall-clock duration of the most recently completed output-stream callback, in
    /// nanoseconds. Updated from the realtime audio thread on every callback; safe to read
    /// from any thread via `.load(Ordering::Relaxed)`. See module docs for context.
    pub last_output_callback_ns: Arc<AtomicU64>,
}

pub fn start_audio_processing(
    audio_tx: Sender<i16>,
) -> Result<AudioSetup, Box<dyn std::error::Error>> {
    let host = cpal::default_host();

    let input_device = host.default_input_device()
        .ok_or("Nenhum dispositivo de entrada de áudio encontrado")?;
    let output_device = host.default_output_device()
        .ok_or("Nenhum dispositivo de saída de áudio encontrado")?;

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
    let input_stream_config = input_config.config();
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
        _ => return Err("Formato de amostra de entrada não suportado pelo CPAL".into()),
    };

    input_stream.play()?;

    // Canal para passar atualizações de pedais para a thread de reprodução
    let (pedal_tx, pedal_rx) = bounded::<Vec<PedalDescription>>(8);

    let last_output_callback_ns = Arc::new(AtomicU64::new(0));

    let output_stream = build_output_stream(
        &output_device,
        output_config,
        input_rx.clone(),
        audio_tx.clone(),
        pedal_rx.clone(),
        Vec::new(),
        last_output_callback_ns.clone(),
    )?;

    output_stream.play()?;

    let configured_latency_secs =
        GLOBAL_CONFIG.buffer_size as f32 / GLOBAL_CONFIG.sample_rate;

    Ok(AudioSetup {
        input_stream,
        output_stream,
        input_tx,
        pedal_tx,
        input_rx,
        pedal_rx,
        audio_tx,
        configured_latency_secs,
        last_output_callback_ns,
    })
}

/// Builds the output stream: drains pedal-chain updates, pulls processed samples off
/// `input_rx`, and forwards the result to `audio_tx` for visualization.
fn build_output_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    input_rx: Receiver<i16>,
    audio_tx: Sender<i16>,
    pedal_rx: Receiver<Vec<PedalDescription>>,
    initial_pedals: Vec<PedalDescription>,
    last_output_callback_ns: Arc<AtomicU64>,
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let sample_format = config.sample_format();
    let stream_config = config.config();
    let channels = stream_config.channels;

    let mut pedal_chain = PedalChain::from_description(initial_pedals);

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            stream_config,
            move |data: &mut [i16], _| {
                let callback_start = Instant::now();

                for msg in pedal_rx.try_iter() {
                    pedal_chain = PedalChain::from_description(msg);
                }

                for frame in data.chunks_mut(channels as usize) {
                    let processed = if let Ok(in_sample) = input_rx.try_recv() {
                        let out_sample = pedal_chain.process_sample(in_sample);
                        let _ = audio_tx.try_send(out_sample);
                        out_sample
                    } else {
                        0
                    };

                    for channel_sample in frame.iter_mut() {
                        *channel_sample = processed;
                    }
                }

                last_output_callback_ns.store(
                    callback_start.elapsed().as_nanos() as u64,
                    Ordering::Relaxed,
                );
            },
            |err| eprintln!("Erro no output stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => device.build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
                let callback_start = Instant::now();

                for msg in pedal_rx.try_iter() {
                    pedal_chain = PedalChain::from_description(msg);
                }

                for frame in data.chunks_mut(channels as usize) {
                    let processed_i16 = if let Ok(in_sample) = input_rx.try_recv() {
                        let out_sample = pedal_chain.process_sample(in_sample);
                        let _ = audio_tx.try_send(out_sample);
                        out_sample
                    } else {
                        0
                    };

                    let processed_f32 = processed_i16 as f32 / i16::MAX as f32;
                    for channel_sample in frame.iter_mut() {
                        *channel_sample = processed_f32;
                    }
                }

                last_output_callback_ns.store(
                    callback_start.elapsed().as_nanos() as u64,
                    Ordering::Relaxed,
                );
            },
            |err| eprintln!("Erro no output stream (F32): {}", err),
            None
        )?,
        _ => return Err("Formato de amostra de saída não suportado pelo CPAL".into()),
    };

    Ok(stream)
}

/// Finds the named output device, builds a fresh output stream for it (seeded with
/// `initial_pedals` so the pedal chain doesn't reset to empty), and starts it playing.
///
/// Kept API-compatible with existing callers (`guitar_core::frontend::gui`'s device-switching
/// flow), so it doesn't take/return the `last_output_callback_ns` handle from `AudioSetup`;
/// a fresh counter is created internally for the new stream's callback and simply isn't
/// surfaced to the caller. If a future overlay needs live timing across device switches too,
/// this would need a signature change on the UI side as well.
pub fn switch_output_device(
    device_name: &str,
    input_rx: Receiver<i16>,
    audio_tx: Sender<i16>,
    pedal_rx: Receiver<Vec<PedalDescription>>,
    initial_pedals: Vec<PedalDescription>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let devices = host.output_devices().map_err(|e| e.to_string())?;
    let device = devices
        .into_iter()
        .find(|d| d.to_string() == device_name)
        .ok_or_else(|| "Dispositivo de saída não encontrado".to_string())?;

    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;
    let config = find_best_config(&device, target_sample_rate, false).map_err(|e| e.to_string())?;

    let last_output_callback_ns = Arc::new(AtomicU64::new(0));
    let stream = build_output_stream(
        &device,
        config,
        input_rx,
        audio_tx,
        pedal_rx,
        initial_pedals,
        last_output_callback_ns,
    )
    .map_err(|e| e.to_string())?;

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
