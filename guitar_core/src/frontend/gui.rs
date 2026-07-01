use cpal;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam::channel::{Receiver, Sender};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use strum::IntoEnumIterator;

use crate::{
    backend::capture::{AudioSetup, find_best_config},
    frontend::{
        lib::fft::compute_fft,
        ui::pedals::{PedalAction, render_pedal_ui},
    },
    shared::{config::GLOBAL_CONFIG, pedals::PedalDescription},
};

const BUFFER_SIZE: usize = 2048;
const FFT_FREQ_RESOLUTION: f64 = GLOBAL_CONFIG.sample_rate as f64 / BUFFER_SIZE as f64;

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

pub struct AudioApp {
    pub audio_rx: Receiver<i16>,
    pub pedal_tx: Sender<Vec<PedalDescription>>,
    buffer: VecDeque<f64>,
    pedal_chain: Vec<PedalDescription>,
    available_inputs: Vec<String>,
    selected_input: String,
    input_stream: Option<cpal::Stream>,
    _output_stream: cpal::Stream,
    input_tx: Sender<i16>,
    switching_rx: Option<Receiver<Result<cpal::Stream, String>>>,
    refreshing_devices_rx: Option<Receiver<Vec<String>>>,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, setup: AudioSetup) -> Self {
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
        let selected_input = host.default_input_device()
            .map(|d| d.to_string())
            .unwrap_or_default();

        // Garante que o dispositivo selecionado inicialmente está na lista
        if !selected_input.is_empty() && !available_inputs.contains(&selected_input) {
            available_inputs.push(selected_input.clone());
        }

        Self {
            audio_rx,
            pedal_tx: setup.pedal_tx,
            buffer: VecDeque::with_capacity(BUFFER_SIZE),
            pedal_chain: Vec::new(),
            available_inputs,
            selected_input,
            input_stream: Some(setup.input_stream),
            _output_stream: setup.output_stream,
            input_tx: setup.input_tx,
            switching_rx: None,
            refreshing_devices_rx: None,
        }
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Consumes samples
        while let Ok(sample) = self.audio_rx.try_recv() {
            if self.buffer.len() >= BUFFER_SIZE {
                self.buffer.pop_front();
            }
            self.buffer.push_back(sample as f64 / i16::MAX as f64);
        }

        // Verifica canais assíncronos
        if let Some(ref rx) = self.switching_rx {
            if let Ok(res) = rx.try_recv() {
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
        }

        if let Some(ref rx) = self.refreshing_devices_rx {
            if let Ok(list) = rx.try_recv() {
                self.available_inputs = list;
                // Garante que a seleção atual continua na lista
                if !self.selected_input.is_empty() && !self.available_inputs.contains(&self.selected_input) {
                    self.available_inputs.push(self.selected_input.clone());
                }
                self.refreshing_devices_rx = None;
            }
        }

        let mut pedal_chain_updated = false;

        // Side panel for controls
        egui::SidePanel::right("controls")
            .resizable(true)
            .min_width(120.0)
            .show(ctx, |ui| {
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

                                        // Dispara a troca em background
                                        let (tx, rx) = crossbeam::channel::bounded(1);
                                        let selected_input = self.selected_input.clone();
                                        let input_tx = self.input_tx.clone();

                                        std::thread::spawn(move || {
                                            let res = (|| -> Result<cpal::Stream, String> {
                                                let host = cpal::default_host();
                                                let devices = host.input_devices().map_err(|e| e.to_string())?;
                                                let dev = devices.into_iter()
                                                    .find(|d| d.to_string() == selected_input)
                                                    .ok_or_else(|| "Dispositivo não encontrado".to_string())?;
                                                
                                                let target_sample_rate = GLOBAL_CONFIG.sample_rate as u32;
                                                let config = find_best_config(&dev, target_sample_rate, true)
                                                    .map_err(|e| e.to_string())?;
                                                let format = config.sample_format();
                                                let stream_cfg = config.config();
                                                let channels = stream_cfg.channels;

                                                let stream = match format {
                                                    cpal::SampleFormat::I16 => dev.build_input_stream(
                                                        stream_cfg,
                                                        move |data: &[i16], _| {
                                                            for frame in data.chunks(channels as usize) {
                                                                if let Some(&sample) = frame.first() {
                                                                    let _ = input_tx.send(sample);
                                                                }
                                                            }
                                                        },
                                                        |err| eprintln!("Erro no input stream (I16): {}", err),
                                                        None
                                                    ).map_err(|e| e.to_string())?,
                                                    cpal::SampleFormat::F32 => dev.build_input_stream(
                                                        stream_cfg,
                                                        move |data: &[f32], _| {
                                                            for frame in data.chunks(channels as usize) {
                                                                if let Some(&sample) = frame.first() {
                                                                    let sample_i16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                                                                    let _ = input_tx.send(sample_i16);
                                                                }
                                                            }
                                                        },
                                                        |err| eprintln!("Erro no input stream (F32): {}", err),
                                                        None
                                                    ).map_err(|e| e.to_string())?,
                                                    _ => return Err("Formato de amostra de entrada não suportado".to_string()),
                                                };

                                                stream.play().map_err(|e| e.to_string())?;
                                                Ok(stream)
                                            })();
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
                                    self.pedal_chain.push(pedal);
                                }
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(available.x, available.y * 0.8),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let mut to_remove = Vec::new();
                            for (i, pedal) in self.pedal_chain.iter_mut().enumerate() {
                                match render_pedal_ui(ui, pedal) {
                                    PedalAction::None => {}
                                    PedalAction::Updated => {
                                        pedal_chain_updated = true;
                                    }
                                    PedalAction::Deleted => {
                                        to_remove.push(i);
                                        pedal_chain_updated = true;
                                    }
                                }
                            }

                            // Remove pedais do fim pro início pra não invalidar índices
                            for i in to_remove.into_iter().rev() {
                                self.pedal_chain.remove(i);
                            }
                        },
                    );
                })
            });

        // Central panel for graphs
        egui::CentralPanel::default().show(ctx, |ui| {
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

            if pedal_chain_updated {
                let _ = self.pedal_tx.send(self.pedal_chain.clone());
            }
        });

        ctx.request_repaint();
    }
}
