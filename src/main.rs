//! Um monitor de áudio de baixa latência que captura de um dispositivo ALSA,
//! converte o sinal de mono para estéreo e o reproduz na saída padrão.

mod buffer;
mod capture;
mod gui;

use std::error::Error;
use std::thread;

fn main() -> Result<(), Box<dyn Error>> {
    let audio_buffer = buffer::AudioBuffer::new();

    let (capture_pcm, playback_pcm) = capture::init_capture(
        capture::CaptureConfig {
            device_name: String::from("hw:2,0"),
        },
        capture::PlaybackConfig {
            device_name: String::from("default"),
            channels: 2,
            sample_rate: 48000,
            period_size: 256,
            buffer_size: 1024,
        },
    )?;
    thread::spawn(|| {
        println!("Thread de áudio iniciada.");
        if let Err(e) = capture::playback(capture_pcm, playback_pcm, audio_buffer) {
            eprintln!("Erro na thread de áudio: {}", e);
        }
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(gui::AudioApp::new(audio_buffer)))),
    )?;
    Ok(())
}
