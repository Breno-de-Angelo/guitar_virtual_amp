use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use crate::buffer::AudioBuffer;

const TIME_DOMAIN_WAVE_BUFFER_SIZE: usize = 2048;

struct SharedAudioData {
    waveform: Vec<i16>,
    fft_magnitudes: Vec<f32>,
}

impl Default for SharedAudioData {
    fn default() -> Self {
        Self {
            waveform: Vec::new(),
            fft_magnitudes: Vec::new(),
        }
    }
}

pub struct AudioApp {
    pub audio_data: AudioBuffer<f64>,
}

impl AudioApp {
    pub fn new(audio_data: AudioBuffer<f64>) -> Self {
        Self { audio_data }
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Analisador de Áudio em Tempo Real");
            ui.separator();

            ui.label("Forma de Onda (Tempo)");

            let (part1, part2) = self.audio_data.latest(TIME_DOMAIN_WAVE_BUFFER_SIZE);
            let wave_points: PlotPoints = part1
                .iter()
                .chain(part2.iter())
                .enumerate()
                .map(|(i, &s)| [i as f64, s])
                .collect();

            let line = Line::new("Sound Wave", wave_points);
            Plot::new("waveform")
                .view_aspect(2.0)
                .height(200.0)
                .show(ui, |plot_ui| {
                    plot_ui.line(line);
                });
        });

        ctx.request_repaint();
    }
}

// impl eframe::App for AudioApp {
//     fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
//         egui::CentralPanel::default().show(ctx, |ui| {
//             ui.heading("Analisador de Áudio em Tempo Real");
//             ui.separator();

//             if let Ok(data) = self.audio_data.try_lock() {
//                 ui.horizontal(|ui| {
//                     // --- Gráfico da Forma de Onda ---
//                     ui.group(|ui| {
//                         ui.label("Forma de Onda (Tempo)");
//                         let wave_points: PlotPoints = data
//                             .waveform
//                             .iter()
//                             .enumerate()
//                             .map(|(i, &s)| [i as f64, s as f64])
//                             .collect();
//                         let line = Line::new("Sound Wave", wave_points);
//                         Plot::new("waveform")
//                             .view_aspect(2.0)
//                             .height(200.0)
//                             .show(ui, |plot_ui| {
//                                 plot_ui.line(line);
//                             });
//                     });

//                     // --- Gráfico da FFT ---
//                     ui.group(|ui| {
//                         ui.label("Espectro de Frequência (FFT)");
//                         let fft_points: PlotPoints = data
//                             .fft_magnitudes
//                             .iter()
//                             .enumerate()
//                             .map(|(i, &mag)| {
//                                 // Mapeia o índice do bin da FFT para a frequência real
//                                 let freq = (i as f64 * SAMPLE_RATE as f64) / FFT_SIZE as f64;
//                                 [freq, mag as f64]
//                             })
//                             .collect();

//                         let line = Line::new("FFT", fft_points);
//                         Plot::new("fft_spectrum")
//                             .view_aspect(2.0)
//                             .height(200.0)
//                             .show(ui, |plot_ui| {
//                                 plot_ui.line(line);
//                             });
//                     });
//                 });
//             }
//         });

//         ctx.request_repaint();
//     }
// }
