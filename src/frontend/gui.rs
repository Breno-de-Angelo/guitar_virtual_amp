use crossbeam::channel::{Receiver, Sender};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use strum::IntoEnumIterator;

use crate::{
    frontend::{
        lib::fft::compute_fft,
        ui::pedals::{PedalAction, render_pedal_ui},
    },
    shared::pedals::PedalDescription,
};

const BUFFER_SIZE: usize = 2048;
const SAMPLE_RATE: f64 = 48000.0;
const FFT_FREQ_RESOLUTION: f64 = SAMPLE_RATE / BUFFER_SIZE as f64;
// Buffer time = BUFFER_SIZE / f = 42,67 ms

pub struct AudioApp {
    pub audio_rx: Receiver<i16>,
    pub control_tx: Sender<Vec<PedalDescription>>,
    buffer: VecDeque<f64>,
    pedal_chain: Vec<PedalDescription>,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, control_tx: Sender<Vec<PedalDescription>>) -> Self {
        Self {
            audio_rx,
            control_tx,
            buffer: VecDeque::with_capacity(BUFFER_SIZE),
            pedal_chain: Vec::new(),
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

        let mut pedal_chain_updated = false;

        // Side panel for buttons
        egui::SidePanel::right("controls")
            .resizable(true)
            .min_width(120.0)
            .show(ctx, |ui| {
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
                self.control_tx.send(self.pedal_chain.clone()).unwrap();
            }
        });

        ctx.request_repaint();
    }
}
