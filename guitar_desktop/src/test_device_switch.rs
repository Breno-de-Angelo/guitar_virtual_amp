use crossbeam::channel::bounded;
use std::thread;
use std::time::Duration;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use guitar_core::backend::capture::{start_audio_processing, find_best_config};
use guitar_core::shared::config::GLOBAL_CONFIG;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Teste de Troca de Dispositivo CLI (Headless) ===");

    let (audio_tx, audio_rx) = bounded(1024);

    println!("Iniciando processamento de áudio padrão...");
    let setup = start_audio_processing(audio_tx)?;
    println!("Stream inicial iniciado. Dispositivos ativos:");
    
    let host = cpal::default_host();
    let default_input = host.default_input_device().map(|d| d.to_string()).unwrap_or_default();
    println!("  - Entrada Inicial: {}", default_input);

    // Canal receptor de áudio em background para evitar buffer cheio
    thread::spawn(move || {
        while let Ok(_) = audio_rx.recv() {
            // Apenas descarta as amostras recebidas
        }
    });

    thread::sleep(Duration::from_secs(2));

    println!("Buscando dispositivos de entrada disponíveis...");
    let devices = host.input_devices()?;
    let mut usb_device_name = None;

    for dev in devices {
        let name = dev.to_string();
        println!("  -> Detectado: {}", name);
        if name.contains("USB") || name.contains("USB2.0") {
            usb_device_name = Some(name);
        }
    }

    let target_device_name = match usb_device_name {
        Some(name) => {
            println!("Interface USB encontrada: '{}'", name);
            name
        }
        None => {
            println!("Nenhuma interface USB com 'USB' no nome encontrada. Usando o primeiro dispositivo alternativo...");
            let devs = host.input_devices()?;
            let alt = devs.into_iter()
                .map(|d| d.to_string())
                .find(|name| name != &default_input);
            match alt {
                Some(name) => name,
                None => {
                    println!("Nenhum dispositivo alternativo encontrado. Encerrando teste.");
                    return Ok(());
                }
            }
        }
    };

    println!("Iniciando processo de troca de dispositivo para: '{}'", target_device_name);

    println!("[1/5] Parando/Dropping stream de entrada anterior...");
    // Simulando self.input_stream = None;
    let old_stream = setup.input_stream;
    drop(old_stream);
    println!("[1/5] Stream anterior dropado com sucesso!");

    thread::sleep(Duration::from_millis(500));

    println!("[2/5] Buscando objeto do dispositivo '{}'...", target_device_name);
    let dev_list = host.input_devices()?;
    let new_device = dev_list.into_iter().find(|d| d.to_string() == target_device_name);
    
    let dev = match new_device {
        Some(d) => {
            println!("[2/5] Objeto do dispositivo obtido.");
            d
        }
        None => {
            return Err("Dispositivo alvo desapareceu da lista!".into());
        }
    };

    println!("[3/5] Buscando melhor configuração para o dispositivo...");
    let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;
    let config = find_best_config(&dev, target_sample_rate, true)?;
    let format = config.sample_format();
    let stream_cfg = config.config();
    let channels = stream_cfg.channels;
    let tx = setup.input_tx.clone();
    println!("[3/5] Configuração selecionada: Format={:?}, Channels={}, SampleRate={}", format, channels, stream_cfg.sample_rate);

    println!("[4/5] Construindo novo stream de entrada...");
    let new_stream = match format {
        cpal::SampleFormat::I16 => dev.build_input_stream(
            stream_cfg,
            move |data: &[i16], _| {
                for frame in data.chunks(channels as usize) {
                    if let Some(&sample) = frame.first() {
                        let _ = tx.send(sample);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (I16): {}", err),
            None
        )?,
        cpal::SampleFormat::F32 => dev.build_input_stream(
            stream_cfg,
            move |data: &[f32], _| {
                for frame in data.chunks(channels as usize) {
                    if let Some(&sample) = frame.first() {
                        let sample_i16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        let _ = tx.send(sample_i16);
                    }
                }
            },
            |err| eprintln!("Erro no input stream (F32): {}", err),
            None
        )?,
        _ => return Err("Formato não suportado".into()),
    };
    println!("[4/5] Novo stream construído com sucesso.");

    println!("[5/5] Iniciando reprodução do novo stream...");
    new_stream.play()?;
    println!("[5/5] Novo stream está tocando!");

    println!("Troca de dispositivo concluída com sucesso! Mantendo ativo por 3 segundos para testar...");
    thread::sleep(Duration::from_secs(3));
    println!("Teste finalizado com sucesso!");

    Ok(())
}
