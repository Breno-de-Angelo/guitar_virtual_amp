use crossbeam::channel::{Receiver, Sender};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use num_complex::Complex;
use rustfft::FftPlanner;
use std::collections::VecDeque;

use crate::shared::pedals::{AmpParams, PedalDescription, ReverbParams};

const BUFFER_SIZE: usize = 2048;
const SAMPLE_RATE: f64 = 48000.0;
const FFT_FREQ_RESOLUTION: f64 = SAMPLE_RATE / BUFFER_SIZE as f64;
// Buffer time = BUFFER_SIZE / f = 42,67 ms

pub struct AudioApp {
    pub audio_rx: Receiver<i16>,
    pub control_tx: Sender<Vec<PedalDescription>>,
    buffer: VecDeque<f64>,
    buffer_size: usize,
    pedal_chain: Vec<PedalDescription>,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, control_tx: Sender<Vec<PedalDescription>>) -> Self {
        Self {
            audio_rx,
            control_tx,
            buffer: VecDeque::with_capacity(BUFFER_SIZE),
            buffer_size: BUFFER_SIZE,
            pedal_chain: Vec::new(),
        }
    }

    fn compute_fft(&self) -> Vec<f64> {
        let n = self.buffer.len().next_power_of_two(); // tamanho da FFT
        if n == 0 {
            return Vec::new();
        }

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);

        let mut input: Vec<Complex<f64>> = self
            .buffer
            .iter()
            .cloned()
            .map(|x| Complex { re: x, im: 0.0 })
            .collect();
        input.resize(n, Complex { re: 0.0, im: 0.0 });

        let mut spectrum = input.clone();
        fft.process(&mut spectrum);

        spectrum[..n / 2].iter().map(|c| c.norm()).collect()
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Consumes samples
        while let Ok(sample) = self.audio_rx.try_recv() {
            if self.buffer.len() >= self.buffer_size {
                self.buffer.pop_front();
            }
            self.buffer.push_back(sample as f64 / i16::MAX as f64);
        }

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
                            let mut pedal_chain_updated = false;
                            if ui.button("Add AMP").clicked() {
                                pedal_chain_updated = true;
                                self.pedal_chain
                                    .push(PedalDescription::Amp(AmpParams::new(2.0)));
                            }
                            if ui.button("Add Reverb").clicked() {
                                pedal_chain_updated = true;
                                self.pedal_chain
                                    .push(PedalDescription::Reverb(ReverbParams::new(0.8)));
                            }

                            if pedal_chain_updated {
                                self.control_tx.send(self.pedal_chain.clone()).unwrap();
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(available.x, available.y * 0.8),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            self.pedal_chain.iter_mut().for_each(|&mut pedal| {
                                // ui.add()
                            });
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
                        let spectrum = self.compute_fft();
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

        ctx.request_repaint();
    }
}
