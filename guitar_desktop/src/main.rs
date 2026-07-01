use crossbeam::channel::bounded;
use std::error::Error;

use guitar_core::{
    backend::capture::start_audio_processing,
    frontend::gui::AudioApp,
};

const BUFFER_SIZE: usize = 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let (audio_tx, audio_rx) = bounded(BUFFER_SIZE);

    // Inicializa o stream de áudio via CPAL
    let audio_setup = start_audio_processing(audio_tx)?;
    println!("Stream de áudio CPAL iniciado no Desktop.");

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(AudioApp::new(audio_rx, audio_setup)))),
    )?;
    Ok(())
}
