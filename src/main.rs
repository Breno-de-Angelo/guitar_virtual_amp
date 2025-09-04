//! Um monitor de áudio de baixa latência que captura de um dispositivo ALSA,
//! converte o sinal de mono para estéreo e o reproduz na saída padrão.

mod playback;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, Error, ValueOr};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::error::Error as StdError;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::playback::playback;

// Constantes de Configuração de Áudio
const CAPTURE_DEVICE: &str = "hw:2,0";
const SAMPLE_RATE: u32 = 48_000;
const PERIOD_SIZE_FRAMES: i64 = 256;
const BUFFER_SIZE_FRAMES: i64 = 1024;

struct AudioApp {
    // Buffer compartilhado que a thread coletora escreve e a GUI lê.
    audio_buffer: Arc<Mutex<Vec<i16>>>,
}

impl AudioApp {
    fn new(audio_buffer: Arc<Mutex<Vec<i16>>>) -> Self {
        Self { audio_buffer }
    }
}

// --- Implementação da Lógica da GUI ---
impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Visualizador de Áudio em Tempo Real");
            ui.separator();

            // Usamos `try_lock` para não bloquear a GUI se a outra thread estiver
            // escrevendo no buffer. Se não conseguir o lock, simplesmente pulamos este frame.
            if let Ok(buffer) = self.audio_buffer.try_lock() {
                // Converte nosso buffer de áudio em pontos para o gráfico.
                // O eixo X é o índice da amostra, o Y é o valor da amostra.
                let points: PlotPoints = buffer
                    .iter()
                    .enumerate()
                    .map(|(i, &sample)| [i as f64, sample as f64])
                    .collect();

                let line = Line::new("Sound Wave", points);

                // Cria o widget do gráfico.
                Plot::new("audio_waveform")
                    .view_aspect(2.0)
                    .show(ui, |plot_ui| {
                        plot_ui.line(line);
                    });
            }
        });

        // Pede ao egui para redesenhar a tela continuamente para criar a animação.
        ctx.request_repaint();
    }
}

fn main() -> Result<(), Box<dyn StdError>> {
    let (tx, rx) = mpsc::channel::<Vec<i16>>();
    let shared_buffer = Arc::new(Mutex::new(Vec::new()));
    let capture_pcm = setup_pcm(CAPTURE_DEVICE, Direction::Capture, 1)?;
    let playback_pcm = PCM::new("default", Direction::Playback, false)
        .or_else(|_| PCM::new("pulse", Direction::Playback, false))
        .map_err(|e| format!("Falha ao abrir dispositivo de playback: {}", e))?;
    let playback_pcm = setup_pcm_from_existing(playback_pcm, 2)?;
    configure_sw_params(&playback_pcm)?;

    // --- 3. Executar o Loop Principal de Monitoramento ---
    println!(
        "Monitorando: '{}' (mono) -> Saída Padrão (estéreo) @ {}Hz.",
        CAPTURE_DEVICE, SAMPLE_RATE
    );
    println!(
        "Período de {} frames. Pressione Ctrl+C para sair.",
        PERIOD_SIZE_FRAMES
    );

    thread::spawn(move || {
        println!("Thread de áudio iniciada.");
        // Passamos os PCMs e o transmissor `tx`
        if let Err(e) = playback(capture_pcm, playback_pcm, tx) {
            eprintln!("Erro na thread de áudio: {}", e);
        }
    });

    let buffer_clone = Arc::clone(&shared_buffer);
    thread::spawn(move || {
        // Esta thread simplesmente recebe do canal e atualiza o estado compartilhado.
        while let Ok(audio_data) = rx.recv() {
            if let Ok(mut buffer) = buffer_clone.lock() {
                *buffer = audio_data;
            }
        }
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(AudioApp::new(shared_buffer)))),
    )?;
    Ok(())
}

/// Configura um novo dispositivo PCM (captura ou playback) com os parâmetros definidos.
fn setup_pcm(
    device_name: &str,
    direction: Direction,
    channels: u32,
) -> Result<PCM, Box<dyn StdError>> {
    let pcm = PCM::new(device_name, direction, false)
        .map_err(|e| format!("Falha ao abrir PCM '{}': {}", device_name, e))?;
    setup_pcm_from_existing(pcm, channels)
}

/// Aplica as configurações de hardware a um dispositivo PCM já aberto.
fn setup_pcm_from_existing(pcm: PCM, channels: u32) -> Result<PCM, Box<dyn StdError>> {
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(Format::s16())?;
        hwp.set_channels(channels)?;
        hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
        hwp.set_period_size(PERIOD_SIZE_FRAMES, ValueOr::Nearest)?;
        hwp.set_buffer_size(BUFFER_SIZE_FRAMES)?;
        pcm.hw_params(&hwp)?;
    }
    Ok(pcm)
}

/// Configura os parâmetros de software do PCM de playback para baixa latência.
fn configure_sw_params(pcm: &PCM) -> Result<(), Error> {
    let swp = pcm.sw_params_current()?;
    // Começa a tocar assim que houver o mínimo de dados no buffer.
    swp.set_start_threshold(1)?;
    // Acorda o processo quando houver espaço para pelo menos um frame.
    swp.set_avail_min(1)?;
    pcm.sw_params(&swp)
}
