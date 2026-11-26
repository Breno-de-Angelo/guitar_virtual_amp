#![cfg(target_os = "android")]

use crossbeam::channel::bounded;
use guitar_core::{
    backend::capture::{AudioSetup, start_audio_processing},
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

    // `dirs::data_dir()` is `None` on Android; TONE3000 downloads and the
    // sign-in session go to the app's private internal storage instead.
    if let Some(path) = app.internal_data_path() {
        guitar_core::shared::paths::set_app_data_dir(path);
    }

    let (audio_tx, audio_rx) = bounded(guitar_core::frontend::gui::VISUALIZER_CHANNEL_SAMPLES);

    // Inicializa o áudio via CPAL (AAudio/OpenSL ES). Sem áudio o app ainda
    // abre, com um aviso e o botão "Retry".
    let audio_setup = match start_audio_processing(audio_tx.clone()) {
        Ok(setup) => {
            log::info!("Stream de áudio CPAL iniciado com sucesso.");
            setup
        }
        Err(e) => {
            log::error!("Erro ao iniciar stream de áudio CPAL: {:?}", e);
            AudioSetup::without_audio(audio_tx)
        }
    };
    // Settings live in the app's private storage (set above).
    let settings = guitar_core::shared::settings::load_settings();

    let mut options = eframe::NativeOptions::default();
    options.android_app = Some(app);
    if let Err(e) = eframe::run_native(
        "GuitarVirtualAmp",
        options,
        Box::new(|_cc| {
            Ok(Box::new(AudioApp::new(audio_rx, audio_setup, settings)))
        }),
    ) {
        log::error!("Erro ao executar eframe no Android: {:?}", e);
    }
}
