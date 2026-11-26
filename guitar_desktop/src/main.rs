use crossbeam::channel::bounded;
use std::error::Error;

use guitar_core::{
    backend::capture::{AudioSetup, start_audio_processing_with_devices},
    frontend::gui::{AudioApp, VISUALIZER_CHANNEL_SAMPLES},
    shared::{config::GLOBAL_CONFIG, settings::load_settings},
};

fn main() -> Result<(), Box<dyn Error>> {
    let (audio_tx, audio_rx) = bounded(VISUALIZER_CHANNEL_SAMPLES);

    // Phase 3.4: restore last-used input/output device (falls back to the
    // host default if unavailable), buffer size, and last-loaded preset
    // across restarts.
    let settings = load_settings();
    let buffer_frames = settings
        .buffer_frames
        .unwrap_or(GLOBAL_CONFIG.buffer_size as u32);

    // Without a working audio device the app still opens, with an error
    // banner and a Retry button (devices can be picked on the Settings page).
    let audio_setup = match start_audio_processing_with_devices(
        audio_tx.clone(),
        settings.input_device.as_deref(),
        settings.output_device.as_deref(),
        buffer_frames,
    ) {
        Ok(setup) => {
            println!("Stream de áudio CPAL iniciado no Desktop.");
            setup
        }
        Err(e) => {
            eprintln!("Não foi possível abrir o áudio: {e}");
            AudioSetup::without_audio(audio_tx)
        }
    };

    // Window size/position persistence is handled by eframe's "persistence"
    // feature (see Cargo.toml), keyed off this app id -- no app code needed
    // beyond enabling the feature and leaving `persist_window` at its default.
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Guitar Virtual Amp")
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([360.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Guitar Virtual Amp",
        options,
        Box::new(|_cc| Ok(Box::new(AudioApp::new(audio_rx, audio_setup, settings)))),
    )?;
    Ok(())
}
