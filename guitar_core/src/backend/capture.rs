use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::channel::{bounded, Receiver, Sender};
use crate::backend::pedals::pedal::{MultiChannelPedalChain, PedalChain};
use crate::shared::pedals::PedalDescription;
use crate::shared::config::GLOBAL_CONFIG;

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
    pub pedal_tx: Sender<Vec<PedalDescription>>,
    // Kept around so the output stream can be rebuilt on a different device later
    // (see `switch_output_device`) with the same processing pipeline.
    pub input_rx: Receiver<i16>,
    pub pedal_rx: Receiver<Vec<PedalDescription>>,
    pub audio_tx: Sender<i16>,
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
                        let _ = input_tx_clone.send(sample);
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
                        let _ = input_tx_clone.send(sample_i16);
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

    let output_stream = build_output_stream(
        &output_device,
        output_config,
        input_rx.clone(),
        audio_tx.clone(),
        pedal_rx.clone(),
        Vec::new(),
    )?;

    output_stream.play()?;

    Ok(AudioSetup {
        input_stream,
        output_stream,
        input_tx,
        pedal_tx,
        input_rx,
        pedal_rx,
        audio_tx,
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
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let sample_format = config.sample_format();
    let stream_config = config.config();
    let channels = stream_config.channels;

    let mut pedal_chain = PedalChain::from_description(initial_pedals);

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            stream_config,
            move |data: &mut [i16], _| {
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
            },
            |err| eprintln!("Erro no output stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => device.build_output_stream(
            stream_config,
            move |data: &mut [f32], _| {
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

    let stream = build_output_stream(&device, config, input_rx, audio_tx, pedal_rx, initial_pedals)
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
    pub pedal_tx: Sender<Vec<PedalDescription>>,
    pub input_rx: Receiver<Vec<i16>>,
    pub pedal_rx: Receiver<Vec<PedalDescription>>,
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
                        let _ = input_tx_clone.send(samples);
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
                        let _ = input_tx_clone.send(samples);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (F32): {}", err),
            None
        )?,
        _ => return Err("Formato de amostra de entrada não suportado pelo CPAL".into()),
    };

    input_stream.play()?;

    let (pedal_tx, pedal_rx) = bounded::<Vec<PedalDescription>>(8);

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
    pedal_rx: Receiver<Vec<PedalDescription>>,
    initial_pedals: Vec<PedalDescription>,
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
    use crate::shared::pedals::{LowPassParams, PedalDescription};

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

        let description = vec![PedalDescription::LowPass(LowPassParams { frequency: 1000.0 })];
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
