// //! Um monitor de áudio de baixa latência que captura de um dispositivo ALSA,
// //! converte o sinal de mono para estéreo e o reproduz na saída padrão.

mod capture;
mod gui;
mod pedal_chain;
mod ring_buffer;

use crossbeam::channel::bounded;
use std::error::Error;
use std::thread;

const BUFFER_SIZE: usize = 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let (tx, rx) = bounded(BUFFER_SIZE);

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
        let mut pedal_chain = pedal_chain::PedalChain::new();
        pedal_chain
            .add_pedal(Box::new(pedal_chain::Reverb::new(1.0)))
            .add_pedal(Box::new(pedal_chain::Amp::new(2.0)));

        if let Err(e) = capture::playback(capture_pcm, playback_pcm, tx, pedal_chain) {
            eprintln!("Erro na thread de áudio: {}", e);
        }
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(gui::AudioApp::new(rx)))),
    )?;
    Ok(())
}
