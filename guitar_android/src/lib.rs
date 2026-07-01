#![cfg(target_os = "android")]

use crossbeam::channel::bounded;
use guitar_core::{
    backend::capture::start_audio_processing,
    frontend::gui::AudioApp,
};

#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("GuitarVirtualAmp"),
    );

    log::info!("Aplicativo Android iniciado.");

    let (audio_tx, audio_rx) = bounded(1024);

    // Inicializa o áudio via CPAL (AAudio/OpenSL ES)
    let audio_setup = match start_audio_processing(audio_tx) {
        Ok(setup) => {
            log::info!("Stream de áudio CPAL iniciado com sucesso.");
            setup
        }
        Err(e) => {
            log::error!("Erro crítico ao iniciar stream de áudio CPAL: {:?}", e);
            return;
        }
    };

    let mut options = eframe::NativeOptions::default();
    options.android_app = Some(app);
    if let Err(e) = eframe::run_native(
        "GuitarVirtualAmp",
        options,
        Box::new(|_cc| Ok(Box::new(AudioApp::new(audio_rx, audio_setup)))),
    ) {
        log::error!("Erro ao executar eframe no Android: {:?}", e);
    }
}
