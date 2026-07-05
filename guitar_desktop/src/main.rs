use crossbeam::channel::bounded;
use std::error::Error;

use guitar_core::{
    backend::capture::start_audio_processing_with_devices,
    frontend::gui::AudioApp,
    shared::{config::GLOBAL_CONFIG, settings::load_settings},
};

const BUFFER_SIZE: usize = 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let (audio_tx, audio_rx) = bounded(BUFFER_SIZE);

    // Phase 3.4: restore last-used input/output device (falls back to the
    // host default if unavailable), buffer size, and last-loaded preset
    // across restarts.
    let settings = load_settings();
    let buffer_frames = settings
        .buffer_frames
        .unwrap_or(GLOBAL_CONFIG.buffer_size as u32);

    let audio_setup = start_audio_processing_with_devices(
        audio_tx,
        settings.input_device.as_deref(),
        settings.output_device.as_deref(),
        buffer_frames,
    )?;
    println!("Stream de áudio CPAL iniciado no Desktop.");

    // Window size/position persistence is handled by eframe's "persistence"
    // feature (see Cargo.toml), keyed off this app id -- no app code needed
    // beyond enabling the feature and leaving `persist_window` at its default.
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(AudioApp::new(audio_rx, audio_setup, settings)))),
    )?;
    Ok(())
}
