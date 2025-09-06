use crossbeam::channel::{Receiver, Sender};
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use num_complex::Complex;
use rustfft::FftPlanner;
use std::collections::VecDeque;

use crate::capture::ControlMsg;

const BUFFER_SIZE: usize = 2048;
const SAMPLE_RATE: f64 = 48000.0;
const FFT_FREQ_RESOLUTION: f64 = SAMPLE_RATE / BUFFER_SIZE as f64;
// Buffer time = BUFFER_SIZE / f = 42,67 ms

pub struct AudioApp {
    audio_rx: Receiver<i16>,
    control_tx: Sender<ControlMsg>,
    buffer: VecDeque<f64>,
    buffer_size: usize,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, control_tx: Sender<ControlMsg>) -> Self {
        Self {
            audio_rx,
            control_tx,
            buffer: VecDeque::with_capacity(BUFFER_SIZE),
            buffer_size: BUFFER_SIZE,
        }
    }

    fn compute_fft(&self) -> Vec<f64> {
        let n = self.buffer.len().next_power_of_two(); // tamanho da FFT
        if n == 0 {
            return Vec::new();
        }

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);

        // prepara o vetor de entrada
        let mut input: Vec<Complex<f64>> = self
            .buffer
            .iter()
            .cloned()
            .map(|x| Complex { re: x, im: 0.0 })
            .collect();
        input.resize(n, Complex { re: 0.0, im: 0.0 });

        // saída
        let mut spectrum = input.clone();
        fft.process(&mut spectrum);

        // módulo (normalizado)
        spectrum[..n / 2].iter().map(|c| c.norm()).collect()
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Consome novos samples do canal
        while let Ok(sample) = self.audio_rx.try_recv() {
            if self.buffer.len() >= self.buffer_size {
                self.buffer.pop_front(); // descarta o mais antigo
            }
            self.buffer.push_back(sample as f64 / i16::MAX as f64);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Real Time Audio Analyzer");
            ui.separator();

            ui.label("Time domain (Osciloscope)");

            // Converte buffer em pontos (x=índice, y=valor)
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
                .view_aspect(4.0) // largura maior que altura
                .show(ui, |plot_ui| {
                    plot_ui.line(line);
                });

            ui.label("Frequency analyzer");
            let spectrum = self.compute_fft();
            let fft_points: PlotPoints = spectrum
                .iter()
                .enumerate()
                .map(|(i, &mag)| [i as f64 * FFT_FREQ_RESOLUTION, mag])
                .collect();

            Plot::new("fft")
                .default_x_bounds(0.0, 10_000.0)
                .default_y_bounds(0.0, 300.0)
                // .height(200.0)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new("FFT", fft_points));
                });

            if ui.button("Add AMP with gain 2.0").clicked() {
                self.control_tx.send(ControlMsg::AddAmp(2.0)).unwrap();
            }
        });

        // força redesenho contínuo
        ctx.request_repaint();
    }
}
