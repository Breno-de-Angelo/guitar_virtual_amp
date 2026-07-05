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
use crate::backend::pedals::pedal::{MultiChannelPedalChain, PedalChain};
use crate::shared::pedals::PedalInstance;
use crate::shared::config::GLOBAL_CONFIG;

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
    pub input_stream: cpal::Stream,
    pub output_stream: cpal::Stream,
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
        .ok_or("Nenhum dispositivo de entrada de áudio encontrado")?;
    let output_device = preferred_output_name
        .and_then(|name| {
            host.output_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.to_string() == name))
        })
        .or_else(|| host.default_output_device())
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
        _ => return Err("Formato de amostra de entrada não suportado pelo CPAL".into()),
    };

    input_stream.play()?;

    // Canal para passar atualizações de pedais para a thread de reprodução
    let (pedal_tx, pedal_rx) = bounded::<Vec<PedalInstance>>(8);

    let last_output_callback_ns = Arc::new(AtomicU64::new(0));

    let (output_stream, output_effective_frames) = build_output_stream(
        &output_device,
        output_config,
        buffer_frames,
        input_rx.clone(),
        audio_tx.clone(),
        pedal_rx.clone(),
        Vec::new(),
        last_output_callback_ns.clone(),
    )?;

    output_stream.play()?;

    // Nominal round-trip latency: input-side + output-side buffer, each converted
    // to seconds. Falls back to the requested (unclamped) frame count for whichever
    // side couldn't report a supported range (see `resolve_stream_config`), which
    // matches the previous fixed-`GLOBAL_CONFIG.buffer_size` estimate's precision.
    let configured_latency_secs = (input_effective_frames.unwrap_or(buffer_frames)
        + output_effective_frames.unwrap_or(buffer_frames)) as f32
        / GLOBAL_CONFIG.sample_rate;

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
/// `input_rx`, and forwards the result to `audio_tx` for visualization. Returns the
/// stream plus the buffer size actually applied (see `resolve_stream_config`).
#[allow(clippy::too_many_arguments)]
fn build_output_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    buffer_frames: u32,
    input_rx: Receiver<i16>,
    audio_tx: Sender<i16>,
    pedal_rx: Receiver<Vec<PedalInstance>>,
    initial_pedals: Vec<PedalInstance>,
    last_output_callback_ns: Arc<AtomicU64>,
) -> Result<(cpal::Stream, Option<u32>), Box<dyn std::error::Error>> {
    let sample_format = config.sample_format();
    let (stream_config, effective_frames) = resolve_stream_config(&config, buffer_frames);
    let channels = stream_config.channels;

    let mut pedal_chain = PedalChain::from_description(initial_pedals);

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            stream_config,
            move |data: &mut [i16], _| {
                let callback_start = Instant::now();

                for msg in pedal_rx.try_iter() {
                    pedal_chain.apply_description(msg);
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
                    pedal_chain.apply_description(msg);
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

    Ok((stream, effective_frames))
}

/// Finds the named output device, builds a fresh output stream for it (seeded with
/// `initial_pedals` so the pedal chain doesn't reset to empty), requesting
/// `buffer_frames` as the callback buffer size, and starts it playing.
///
/// Kept API-compatible with existing callers (`guitar_core::frontend::gui`'s device-switching
/// flow), so it doesn't take/return the `last_output_callback_ns` handle from `AudioSetup`;
/// a fresh counter is created internally for the new stream's callback and simply isn't
/// surfaced to the caller. If a future overlay needs live timing across device switches too,
/// this would need a signature change on the UI side as well.
pub fn switch_output_device(
    device_name: &str,
    buffer_frames: u32,
    input_rx: Receiver<i16>,
    audio_tx: Sender<i16>,
    pedal_rx: Receiver<Vec<PedalInstance>>,
    initial_pedals: Vec<PedalInstance>,
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
    let (stream, _effective_frames) = build_output_stream(
        &device,
        config,
        buffer_frames,
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
        .ok_or_else(|| "Dispositivo de entrada não encontrado".to_string())?;

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
        _ => return Err("Formato de amostra de entrada não suportado".to_string()),
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
        .ok_or("Nenhum dispositivo de entrada de áudio encontrado")?;
    let output_device = host.default_output_device()
        .ok_or("Nenhum dispositivo de saída de áudio encontrado")?;

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
        _ => return Err("Formato de amostra de entrada não suportado pelo CPAL".into()),
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
        _ => return Err("Formato de amostra de saída não suportado pelo CPAL".into()),
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
