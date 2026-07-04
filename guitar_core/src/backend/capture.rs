use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::channel::{bounded, Receiver, Sender};
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
