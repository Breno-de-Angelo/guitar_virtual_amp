// //! Um monitor de áudio de baixa latência que captura de um dispositivo ALSA,
// //! converte o sinal de mono para estéreo e o reproduz na saída padrão.

mod capture;
mod gui;
mod pedal_chain;
mod ring_buffer;

use crossbeam::channel::{bounded, unbounded};
use std::error::Error;
use std::thread;

const BUFFER_SIZE: usize = 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let (audio_tx, audio_rx) = bounded(BUFFER_SIZE);
    let (control_tx, control_rx) = unbounded();

    let capture_pcm = capture::init_device(capture::AudioConfig {
        device_name: String::from("hw:2,0"),
        io_select: capture::IOSelect::INPUT,
        channels: 1,
        sample_rate: 48000,
        period_size: 256,
        buffer_size: 1024,
    })?;
    let playback_pcm = capture::init_device(capture::AudioConfig {
        device_name: String::from("default"),
        io_select: capture::IOSelect::OUTPUT,
        channels: 2,
        sample_rate: 48000,
        period_size: 256,
        buffer_size: 1024,
    })?;

    thread::spawn(|| {
        println!("Thread de áudio iniciada.");
        if let Err(e) = capture::playback(capture_pcm, playback_pcm, audio_tx, control_rx) {
            eprintln!("Erro na thread de áudio: {}", e);
        }
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(gui::AudioApp::new(audio_rx, control_tx)))),
    )?;
    Ok(())
}
