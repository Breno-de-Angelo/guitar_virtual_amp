use cpal;
use cpal::traits::HostTrait;
use crossbeam::channel::{Receiver, Sender};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use strum::IntoEnumIterator;

use crate::{
    backend::{
        capture::{AudioSetup, INPUT_BACKLOG_SAMPLES, switch_input_device, switch_output_device},
        pedals::nam,
        song_player::SongPlayer,
    },
    frontend::{
        lib::fft::compute_fft,
        ui::metronome::MetronomePanel,
        ui::pedals::{PedalAction, render_pedal_ui},
        ui::presets::PresetPanel,
        ui::spotify::{SpotifyAction, SpotifyPanel},
        ui::tab_player::TabPlayerPanel,
        ui::tone3000::{LoadedTone, Tone3000Panel, ToneTarget},
        ui::tuner::tuner_ui,
    },
    shared::{
        config::{BUFFER_SIZE_OPTIONS_FRAMES, GLOBAL_CONFIG},
        factory_presets::factory_presets,
        pedals::{CabinetParams, NamParams, PedalDescription, PedalInstance},
        preset,
        settings::{save_settings, AppSettings, PresetRef},
    },
};

const BUFFER_SIZE: usize = 2048;

/// Capacity of the `audio_rx` channel the entry points create. The audio
/// thread drops samples when it's full, so it holds about a second: tab
/// practice mode needs a gap-free signal, and a UI frame can be late.
pub const VISUALIZER_CHANNEL_SAMPLES: usize = 48_000;
const FFT_FREQ_RESOLUTION: f64 = GLOBAL_CONFIG.sample_rate as f64 / BUFFER_SIZE as f64;

/// Which page the central panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainView {
    Analyzer,
    Tabs,
    Spotify,
}

/// Result of a background buffer-size change: (input stream result, output stream result).
type BufferSwitchResult = (Result<cpal::Stream, String>, Result<cpal::Stream, String>);

/// Parses every NAM model the chain uses into the model cache, on the UI
/// thread, so the audio thread doesn't read files when it rebuilds the chain
/// (see `backend::pedals::nam`). Cache hits are a map lookup, so this is
/// cheap to call on every chain update.
fn preload_nam_models(chain: &[PedalInstance]) {
    let paths: Vec<&str> = chain
        .iter()
        .filter_map(|pedal| match &pedal.description {
            PedalDescription::Nam(params) => params.model_path.as_deref(),
            _ => None,
        })
        .collect();
    for path in &paths {
        let _ = nam::preload(path);
    }
    nam::retain_only(&paths);
}

/// Puts a tone downloaded from TONE3000 into the pedal that asked for it.
/// If that pedal was deleted or replaced meanwhile, appends a new pedal
/// instead so the download isn't lost.
fn apply_loaded_tone(chain: &mut Vec<PedalInstance>, loaded: LoadedTone) {
    let target_pedal = chain.get_mut(loaded.pedal_index).map(|p| &mut p.description);
    match (target_pedal, loaded.target) {
        (Some(PedalDescription::Nam(params)), ToneTarget::Nam) => {
            params.model_path = Some(loaded.path);
            params.source = Some(loaded.source);
        }
        (Some(PedalDescription::Cabinet(params)), ToneTarget::Ir) => {
            params.ir_path = Some(loaded.path);
            params.source = Some(loaded.source);
        }
        (_, ToneTarget::Nam) => chain.push(PedalInstance::new(PedalDescription::Nam(NamParams {
            model_path: Some(loaded.path),
            source: Some(loaded.source),
            ..Default::default()
        }))),
        (_, ToneTarget::Ir) => chain.push(PedalInstance::new(PedalDescription::Cabinet(
            CabinetParams {
                ir_path: Some(loaded.path),
                source: Some(loaded.source),
                ..Default::default()
            },
        ))),
    }
}

fn is_valid_input_device(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    if name_lower.contains("discard")
        || name_lower.contains("rate converter")
        || name_lower.contains("speex")
        || name_lower.contains("upmix")
        || name_lower.contains("downmix")
        || name_lower.contains("plugin")
        || name_lower.contains("jack")
        || name_lower.contains("open sound system")
        || name_lower.contains("oss")
        || name_lower.contains("hdmi") // HDMI é apenas para saída
    {
        return false;
    }
    true
}

fn is_valid_output_device(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    !(name_lower.contains("discard")
        || name_lower.contains("rate converter")
        || name_lower.contains("speex")
        || name_lower.contains("upmix")
        || name_lower.contains("downmix")
        || name_lower.contains("plugin")
        || name_lower.contains("jack")
        || name_lower.contains("open sound system")
        || name_lower.contains("oss"))
}

pub struct AudioApp {
    pub audio_rx: Receiver<i16>,
    pub pedal_tx: Sender<Vec<PedalInstance>>,
    buffer: VecDeque<f64>,
    pedal_chain: Vec<PedalInstance>,
    available_inputs: Vec<String>,
    selected_input: String,
    input_stream: Option<cpal::Stream>,
    input_tx: Sender<i16>,
    switching_rx: Option<Receiver<Result<cpal::Stream, String>>>,
    refreshing_devices_rx: Option<Receiver<Vec<String>>>,
    available_outputs: Vec<String>,
    selected_output: String,
    output_stream: Option<cpal::Stream>,
    // Clones of the audio-processing pipeline handles, kept so a new output stream
    // can be rebuilt on demand when the user picks a different output device.
    output_input_rx: Receiver<i16>,
    output_pedal_rx: Receiver<Vec<PedalInstance>>,
    output_audio_tx: Sender<i16>,
    switching_output_rx: Option<Receiver<Result<cpal::Stream, String>>>,
    refreshing_output_devices_rx: Option<Receiver<Vec<String>>>,
    preset_panel: PresetPanel,
    metronome_panel: MetronomePanel,
    tone3000_panel: Tone3000Panel,
    /// Nominal round-trip buffer latency in seconds, computed once at stream
    /// setup (Phase 3.2 latency/CPU meter overlay). See `capture.rs` module docs.
    configured_latency_secs: f32,
    /// Wall-clock duration of the most recently completed output-stream
    /// callback, updated from the realtime audio thread. See `capture.rs`.
    last_output_callback_ns: Arc<AtomicU64>,
    /// Shared with the realtime output callback so it can generate the
    /// metronome click sample-accurately. See `capture.rs`.
    metronome_running: Arc<AtomicBool>,
    metronome_bpm: Arc<AtomicU32>,
    /// Tab-player song engine, shared with the output stream so it can be
    /// handed to a rebuilt stream on device/buffer changes.
    song_player: SongPlayer,
    tab_panel: TabPlayerPanel,
    spotify_panel: SpotifyPanel,
    main_view: MainView,
    /// Index (in `pedal_chain`) of the pedal currently being dragged for
    /// reordering, if any.
    dragged_pedal: Option<usize>,
    /// Requested host-callback buffer size in frames (see
    /// `backend::capture::resolve_stream_config`); changing it tears down and
    /// rebuilds both streams against the currently selected devices.
    buffer_frames: u32,
    /// Non-`None` while a buffer-size change is rebuilding both streams in
    /// the background (mirrors `switching_rx`/`switching_output_rx`).
    switching_buffer_rx: Option<Receiver<BufferSwitchResult>>,
    /// Phase 3.4 persisted config (last-used devices, last-loaded preset).
    /// Re-saved (best-effort) whenever one of those changes.
    settings: AppSettings,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, setup: AudioSetup, settings: AppSettings) -> Self {
        let host = cpal::default_host();
        let mut available_inputs = host.input_devices()
            .map(|devs| {
                let mut list: Vec<String> = devs.into_iter()
                    .map(|d| d.to_string())
                    .filter(|name| is_valid_input_device(name))
                    .collect();
                list.sort();
                list.dedup();
                list
            })
            .unwrap_or_default();
        // Prefer the last-used device from settings (Phase 3.4) if it's still
        // available; the audio stream itself was already opened against this
        // same preference by `start_audio_processing_with_devices`.
        let selected_input = settings
            .input_device
            .clone()
            .filter(|name| available_inputs.contains(name))
            .or_else(|| host.default_input_device().map(|d| d.to_string()))
            .unwrap_or_default();

        // Garante que o dispositivo selecionado inicialmente está na lista
        if !selected_input.is_empty() && !available_inputs.contains(&selected_input) {
            available_inputs.push(selected_input.clone());
        }

        let mut available_outputs = host.output_devices()
            .map(|devs| {
                let mut list: Vec<String> = devs.into_iter()
                    .map(|d| d.to_string())
                    .filter(|name| is_valid_output_device(name))
                    .collect();
                list.sort();
                list.dedup();
                list
            })
            .unwrap_or_default();
        let selected_output = settings
            .output_device
            .clone()
            .filter(|name| available_outputs.contains(name))
            .or_else(|| host.default_output_device().map(|d| d.to_string()))
            .unwrap_or_default();

        if !selected_output.is_empty() && !available_outputs.contains(&selected_output) {
            available_outputs.push(selected_output.clone());
        }

        // Restore the last-loaded preset (Phase 3.4), if any -- best-effort,
        // silently starting with an empty chain if it's gone missing.
        let initial_pedal_chain = match &settings.last_preset {
            Some(PresetRef::User(path)) => preset::load_from_file(path)
                .map(|p| p.pedals)
                .unwrap_or_default(),
            Some(PresetRef::Factory(name)) => factory_presets()
                .into_iter()
                .find(|p| &p.name == name)
                .map(|p| p.pedals)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        if !initial_pedal_chain.is_empty() {
            preload_nam_models(&initial_pedal_chain);
            let _ = setup.pedal_tx.send(initial_pedal_chain.clone());
        }

        Self {
            audio_rx,
            pedal_tx: setup.pedal_tx,
            buffer: VecDeque::with_capacity(BUFFER_SIZE),
            pedal_chain: initial_pedal_chain,
            available_inputs,
            selected_input,
            input_stream: Some(setup.input_stream),
            input_tx: setup.input_tx,
            switching_rx: None,
            refreshing_devices_rx: None,
            available_outputs,
            selected_output,
            output_stream: Some(setup.output_stream),
            output_input_rx: setup.input_rx,
            output_pedal_rx: setup.pedal_rx,
            output_audio_tx: setup.audio_tx,
            switching_output_rx: None,
            refreshing_output_devices_rx: None,
            preset_panel: PresetPanel::new(),
            tone3000_panel: Tone3000Panel::new(),
            metronome_panel: MetronomePanel::new(
                setup.metronome_running.clone(),
                setup.metronome_bpm.clone(),
            ),
            configured_latency_secs: setup.configured_latency_secs,
            last_output_callback_ns: setup.last_output_callback_ns,
            metronome_running: setup.metronome_running,
            metronome_bpm: setup.metronome_bpm,
            tab_panel: TabPlayerPanel::new(setup.song_player.clone(), &settings),
            spotify_panel: SpotifyPanel::new(),
            main_view: if settings.last_tab.is_some() { MainView::Tabs } else { MainView::Analyzer },
            song_player: setup.song_player,
            dragged_pedal: None,
            buffer_frames: settings
                .buffer_frames
                .unwrap_or(GLOBAL_CONFIG.buffer_size as u32),
            switching_buffer_rx: None,
            settings,
        }
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Consumes samples
        let mut fresh: Vec<f32> = Vec::with_capacity(self.audio_rx.len());
        while let Ok(sample) = self.audio_rx.try_recv() {
            if self.buffer.len() >= BUFFER_SIZE {
                self.buffer.pop_front();
            }
            let normalized = sample as f64 / i16::MAX as f64;
            self.buffer.push_back(normalized);
            fresh.push(normalized as f32);
        }
        // Tab practice mode needs every sample, not just the display window.
        self.tab_panel.feed_input(&fresh, self.configured_latency_secs);

        // Verifica canais assíncronos
        if let Some(ref rx) = self.switching_rx
            && let Ok(res) = rx.try_recv()
        {
            match res {
                Ok(stream) => {
                    self.input_stream = Some(stream);
                    println!("Dispositivo de entrada alterado com sucesso de forma assíncrona.");
                }
                Err(e) => {
                    eprintln!("Erro ao alterar dispositivo: {}", e);
                }
            }
            self.switching_rx = None;
        }

        if let Some(ref rx) = self.refreshing_devices_rx
            && let Ok(list) = rx.try_recv()
        {
            self.available_inputs = list;
            // Garante que a seleção atual continua na lista
            if !self.selected_input.is_empty() && !self.available_inputs.contains(&self.selected_input) {
                self.available_inputs.push(self.selected_input.clone());
            }
            self.refreshing_devices_rx = None;
        }

        if let Some(ref rx) = self.switching_output_rx
            && let Ok(res) = rx.try_recv()
        {
            match res {
                Ok(stream) => {
                    self.output_stream = Some(stream);
                    println!("Dispositivo de saída alterado com sucesso de forma assíncrona.");
                }
                Err(e) => {
                    eprintln!("Erro ao alterar dispositivo de saída: {}", e);
                }
            }
            self.switching_output_rx = None;
        }

        if let Some(ref rx) = self.switching_buffer_rx
            && let Ok((input_res, output_res)) = rx.try_recv()
        {
            match input_res {
                Ok(stream) => self.input_stream = Some(stream),
                Err(e) => eprintln!("Erro ao aplicar novo buffer size (entrada): {}", e),
            }
            match output_res {
                Ok(stream) => self.output_stream = Some(stream),
                Err(e) => eprintln!("Erro ao aplicar novo buffer size (saída): {}", e),
            }
            self.switching_buffer_rx = None;
        }

        if let Some(ref rx) = self.refreshing_output_devices_rx
            && let Ok(list) = rx.try_recv()
        {
            self.available_outputs = list;
            if !self.selected_output.is_empty() && !self.available_outputs.contains(&self.selected_output) {
                self.available_outputs.push(self.selected_output.clone());
            }
            self.refreshing_output_devices_rx = None;
        }

        let mut pedal_chain_updated = false;
        let mut tone3000_request = None;

        // Side panel for controls
        egui::SidePanel::right("controls")
            .resizable(true)
            .min_width(120.0)
            .show(ctx, |ui| {
                // Phase 3.2: latency/CPU meter overlay, surfacing the timing
                // instrumentation exposed by `backend::capture::AudioSetup`.
                ui.horizontal(|ui| {
                    let buffer_ms = self.configured_latency_secs * 1000.0;
                    let callback_ns = self.last_output_callback_ns.load(Ordering::Relaxed);
                    let callback_period_ns =
                        (self.configured_latency_secs as f64 * 1_000_000_000.0).max(1.0);
                    let cpu_pct = (callback_ns as f64 / callback_period_ns) * 100.0;
                    let color = if cpu_pct > 80.0 {
                        egui::Color32::RED
                    } else if cpu_pct > 50.0 {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::LIGHT_GREEN
                    };
                    // Audio waiting between the input and output streams, on
                    // top of the device buffers.
                    let queue_ms = INPUT_BACKLOG_SAMPLES.load(Ordering::Relaxed) as f32 * 1000.0
                        / GLOBAL_CONFIG.sample_rate;
                    ui.colored_label(
                        color,
                        format!("buffer: {buffer_ms:.1}ms, queue: {queue_ms:.1}ms, CPU: {cpu_pct:.0}%"),
                    );
                });

                // Buffer-size tuning: lower = lower latency but higher underrun risk,
                // higher = safer but more latency. Requested size is clamped to
                // whatever the device actually supports (`resolve_stream_config`).
                ui.horizontal(|ui| {
                    ui.label("Buffer:");
                    if self.switching_buffer_rx.is_some() {
                        ui.colored_label(egui::Color32::YELLOW, "Applying...");
                    } else {
                        let mut selected = self.buffer_frames;
                        egui::ComboBox::new("buffer_size_select", "")
                            .selected_text(format!("{selected} frames"))
                            .show_ui(ui, |ui| {
                                for &frames in BUFFER_SIZE_OPTIONS_FRAMES {
                                    ui.selectable_value(
                                        &mut selected,
                                        frames,
                                        format!("{frames} frames"),
                                    );
                                }
                            });

                        if selected != self.buffer_frames {
                            self.buffer_frames = selected;
                            // Recompute the nominal latency immediately; the background
                            // rebuild below will correct it if the device clamps the
                            // request to a different actual size.
                            self.configured_latency_secs =
                                2.0 * selected as f32 / GLOBAL_CONFIG.sample_rate;

                            self.settings.buffer_frames = Some(selected);
                            save_settings(&self.settings);

                            self.input_stream = None;
                            self.output_stream = None;

                            let (tx, rx) = crossbeam::channel::bounded(1);
                            let selected_input = self.selected_input.clone();
                            let selected_output = self.selected_output.clone();
                            let input_tx = self.input_tx.clone();
                            let output_input_rx = self.output_input_rx.clone();
                            let output_audio_tx = self.output_audio_tx.clone();
                            let output_pedal_rx = self.output_pedal_rx.clone();
                            let initial_pedals = self.pedal_chain.clone();
                            let metronome_running = self.metronome_running.clone();
                            let metronome_bpm = self.metronome_bpm.clone();
                            let song_player = self.song_player.clone();

                            std::thread::spawn(move || {
                                let input_res =
                                    switch_input_device(&selected_input, selected, input_tx);
                                let output_res = switch_output_device(
                                    &selected_output,
                                    selected,
                                    output_input_rx,
                                    output_audio_tx,
                                    output_pedal_rx,
                                    initial_pedals,
                                    metronome_running,
                                    metronome_bpm,
                                    song_player,
                                );
                                let _ = tx.send((input_res, output_res));
                            });

                            self.switching_buffer_rx = Some(rx);
                        }
                    }
                });
                ui.separator();

                ui.heading("Input Source");
                
                ui.horizontal(|ui| {
                    if self.switching_rx.is_some() {
                        ui.colored_label(egui::Color32::YELLOW, "Conectando...");
                    } else {
                        let combobox = egui::ComboBox::new("input_source_select", "")
                            .selected_text(&self.selected_input)
                            .show_ui(ui, |ui| {
                                for input in &self.available_inputs {
                                    if ui.selectable_value(&mut self.selected_input, input.clone(), input).changed() {
                                        // Para o stream anterior imediatamente
                                        self.input_stream = None;

                                        self.settings.input_device = Some(self.selected_input.clone());
                                        save_settings(&self.settings);

                                        // Dispara a troca em background
                                        let (tx, rx) = crossbeam::channel::bounded(1);
                                        let selected_input = self.selected_input.clone();
                                        let input_tx = self.input_tx.clone();
                                        let buffer_frames = self.buffer_frames;

                                        std::thread::spawn(move || {
                                            let res = switch_input_device(
                                                &selected_input,
                                                buffer_frames,
                                                input_tx,
                                            );
                                            let _ = tx.send(res);
                                        });

                                        self.switching_rx = Some(rx);
                                    }
                                }
                            });
                        
                        if combobox.response.changed() {
                            // Combobox changed
                        }
                    }

                    if self.refreshing_devices_rx.is_some() {
                        ui.colored_label(egui::Color32::LIGHT_BLUE, "🔄");
                    } else if ui.button("🔄").on_hover_text("Atualizar lista de dispositivos").clicked() {
                        let (tx, rx) = crossbeam::channel::bounded(1);
                        std::thread::spawn(move || {
                            let host = cpal::default_host();
                            let list = host.input_devices()
                                .map(|devs| {
                                    let mut l: Vec<String> = devs.into_iter()
                                        .map(|d| d.to_string())
                                        .filter(|name| is_valid_input_device(name))
                                        .collect();
                                    l.sort();
                                    l.dedup();
                                    l
                                })
                                .unwrap_or_default();
                            let _ = tx.send(list);
                        });
                        self.refreshing_devices_rx = Some(rx);
                    }
                });
                ui.separator();

                ui.heading("Output Device");

                ui.horizontal(|ui| {
                    if self.switching_output_rx.is_some() {
                        ui.colored_label(egui::Color32::YELLOW, "Conectando...");
                    } else {
                        egui::ComboBox::new("output_source_select", "")
                            .selected_text(&self.selected_output)
                            .show_ui(ui, |ui| {
                                for output in &self.available_outputs {
                                    if ui.selectable_value(&mut self.selected_output, output.clone(), output).changed() {
                                        // Para o stream anterior imediatamente
                                        self.output_stream = None;

                                        self.settings.output_device = Some(self.selected_output.clone());
                                        save_settings(&self.settings);

                                        // Dispara a troca em background
                                        let (tx, rx) = crossbeam::channel::bounded(1);
                                        let selected_output = self.selected_output.clone();
                                        let input_rx = self.output_input_rx.clone();
                                        let audio_tx = self.output_audio_tx.clone();
                                        let pedal_rx = self.output_pedal_rx.clone();
                                        let initial_pedals = self.pedal_chain.clone();
                                        let buffer_frames = self.buffer_frames;
                                        let metronome_running = self.metronome_running.clone();
                                        let metronome_bpm = self.metronome_bpm.clone();
                                        let song_player = self.song_player.clone();

                                        std::thread::spawn(move || {
                                            let res = switch_output_device(
                                                &selected_output,
                                                buffer_frames,
                                                input_rx,
                                                audio_tx,
                                                pedal_rx,
                                                initial_pedals,
                                                metronome_running,
                                                metronome_bpm,
                                                song_player,
                                            );
                                            let _ = tx.send(res);
                                        });

                                        self.switching_output_rx = Some(rx);
                                    }
                                }
                            });
                    }

                    if self.refreshing_output_devices_rx.is_some() {
                        ui.colored_label(egui::Color32::LIGHT_BLUE, "🔄");
                    } else if ui.button("🔄").on_hover_text("Atualizar lista de dispositivos").clicked() {
                        let (tx, rx) = crossbeam::channel::bounded(1);
                        std::thread::spawn(move || {
                            let host = cpal::default_host();
                            let list = host.output_devices()
                                .map(|devs| {
                                    let mut l: Vec<String> = devs.into_iter()
                                        .map(|d| d.to_string())
                                        .filter(|name| is_valid_output_device(name))
                                        .collect();
                                    l.sort();
                                    l.dedup();
                                    l
                                })
                                .unwrap_or_default();
                            let _ = tx.send(list);
                        });
                        self.refreshing_output_devices_rx = Some(rx);
                    }
                });
                ui.separator();

                ui.collapsing("Presets", |ui| {
                    if let Some((pedals, preset_ref)) = self.preset_panel.ui(ui, &self.pedal_chain) {
                        self.pedal_chain = pedals;
                        pedal_chain_updated = true;
                        self.settings.last_preset = Some(preset_ref);
                        save_settings(&self.settings);
                    }
                });
                ui.separator();

                ui.collapsing("Tuner", |ui| {
                    tuner_ui(ui, &self.buffer);
                });
                ui.separator();

                ui.collapsing("Metronome", |ui| {
                    if self.metronome_panel.ui(ui, &mut self.pedal_chain) {
                        pedal_chain_updated = true;
                    }
                });
                ui.separator();

                ui.heading("Controls");
                ui.separator();

                ui.vertical(|ui| {
                    let available = ui.available_size();

                    ui.allocate_ui_with_layout(
                        egui::vec2(available.x, available.y * 0.2),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            for pedal in PedalDescription::iter() {
                                if ui.button(String::from("Add ") + pedal.name()).clicked() {
                                    pedal_chain_updated = true;
                                    self.pedal_chain.push(PedalInstance::new(pedal));
                                }
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(available.x, available.y * 0.8),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let mut to_remove = Vec::new();
                            let mut drop_target: Option<usize> = None;

                            for (i, pedal) in self.pedal_chain.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    // Drag handle: press-and-drag to reorder the chain,
                                    // matching a physical pedalboard's rearrangeable layout.
                                    let handle =
                                        ui.add(egui::Label::new("☰").sense(egui::Sense::drag()));
                                    if handle.drag_started() {
                                        self.dragged_pedal = Some(i);
                                    }
                                    if self.dragged_pedal.is_some()
                                        && let Some(pointer) = ui.ctx().pointer_interact_pos()
                                        && (handle.rect.y_range().contains(pointer.y)
                                            || ui.min_rect().y_range().contains(pointer.y))
                                    {
                                        drop_target = Some(i);
                                    }

                                    ui.vertical(|ui| {
                                        match render_pedal_ui(ui, pedal) {
                                            PedalAction::None => {}
                                            PedalAction::Updated => {
                                                pedal_chain_updated = true;
                                            }
                                            PedalAction::Deleted => {
                                                to_remove.push(i);
                                                pedal_chain_updated = true;
                                            }
                                            PedalAction::Tone3000(request) => {
                                                tone3000_request = Some((i, request));
                                            }
                                        }
                                    });
                                });
                            }

                            if ui.input(|i| i.pointer.any_released())
                                && let (Some(from), Some(to)) =
                                    (self.dragged_pedal.take(), drop_target)
                                && from != to
                                && from < self.pedal_chain.len()
                            {
                                let pedal = self.pedal_chain.remove(from);
                                let to = to.min(self.pedal_chain.len());
                                self.pedal_chain.insert(to, pedal);
                                pedal_chain_updated = true;
                            }

                            // Remove pedais do fim pro início pra não invalidar índices
                            for i in to_remove.into_iter().rev() {
                                self.pedal_chain.remove(i);
                            }
                        },
                    );
                })
            });

        if let Some((pedal_index, request)) = tone3000_request {
            self.tone3000_panel.handle_request(pedal_index, request);
        }
        if let Some(loaded) = self.tone3000_panel.show(ctx) {
            apply_loaded_tone(&mut self.pedal_chain, loaded);
            pedal_chain_updated = true;
        }

        // Central panel: analyzer graphs, the tab player or Spotify.
        let mut tone_choice = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.main_view, MainView::Analyzer, "📈 Analyzer");
                ui.selectable_value(&mut self.main_view, MainView::Tabs, "🎼 Tabs");
                ui.selectable_value(&mut self.main_view, MainView::Spotify, "🎧 Spotify");
            });
            ui.separator();

            if self.main_view == MainView::Tabs {
                tone_choice = self.tab_panel.ui(ui, &mut self.settings);
                return;
            }
            if self.main_view == MainView::Spotify {
                match self.spotify_panel.ui(ui, &mut self.settings) {
                    Some(SpotifyAction::OpenTab(path)) => {
                        self.tab_panel.open_tab(path);
                        self.main_view = MainView::Tabs;
                    }
                    Some(SpotifyAction::Transcribe { title, artist, track_id, duration_secs }) => {
                        self.tab_panel.start_transcription_for(title, artist, Some(track_id), duration_secs);
                        self.main_view = MainView::Tabs;
                    }
                    None => {}
                }
                return;
            }

            ui.heading("Real Time Audio Analyzer");
            ui.separator();

            ui.label("Time domain (Oscilloscope)");

            ui.vertical(|ui| {
                let available = ui.available_size();
                let half_height = available.y / 2.0;

                // Time domain
                ui.allocate_ui_with_layout(
                    egui::vec2(available.x, half_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let points: PlotPoints = self
                            .buffer
                            .iter()
                            .enumerate()
                            .map(|(i, &y)| [i as f64, y])
                            .collect();
                        let line = Line::new("Sound Wave", points);
                        Plot::new("waveform")
                            .default_x_bounds(0.0, BUFFER_SIZE as f64)
                            .default_y_bounds(-1.0, 1.0)
                            .show(ui, |plot_ui| {
                                plot_ui.line(line);
                            });
                    },
                );

                // FFT
                ui.allocate_ui_with_layout(
                    egui::vec2(available.x, half_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let spectrum = compute_fft(&self.buffer);
                        let fft_points: PlotPoints = spectrum
                            .iter()
                            .enumerate()
                            .map(|(i, &mag)| [i as f64 * FFT_FREQ_RESOLUTION, mag])
                            .collect();
                        Plot::new("fft")
                            .default_x_bounds(0.0, 10_000.0)
                            .default_y_bounds(0.0, 300.0)
                            .show(ui, |plot_ui| {
                                plot_ui.line(Line::new("FFT", fft_points));
                            });
                    },
                );
            });
        });

        // Outside the central panel so it also runs when the Tabs page

        // returns early.

        // A tone suggestion applied on the Tabs page replaces the chain, like
        // loading a preset.
        if let Some((pedals, preset_ref)) = tone_choice {
            self.pedal_chain = pedals;
            pedal_chain_updated = true;
            self.settings.last_preset = Some(preset_ref);
            save_settings(&self.settings);
        }

        if pedal_chain_updated {

            preload_nam_models(&self.pedal_chain);

            let _ = self.pedal_tx.send(self.pedal_chain.clone());

        }

        ctx.request_repaint();
    }
}
