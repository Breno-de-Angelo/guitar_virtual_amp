//! "Analyzer" page: an oscilloscope triggered on rising zero crossings (so
//! a held note stands still) and a log-frequency spectrum in dBFS with
//! peak hold. Both read the same guitar sample buffer as the tuner.

use std::collections::VecDeque;

use eframe::egui::{RichText, Ui};
use egui_plot::{GridMark, Line, Plot, PlotPoints, log_grid_spacer};

use crate::frontend::lib::fft::SpectrumAnalyzer;
use crate::frontend::style::Palette;
use crate::shared::config::GLOBAL_CONFIG;

const TIME_BASES_MS: [f32; 5] = [5.0, 10.0, 20.0, 40.0, 80.0];

pub struct AnalyzerPage {
    analyzer: SpectrumAnalyzer,
    smoothed: Vec<f64>,
    peak_hold: Vec<f64>,
    frozen: Option<(Vec<f64>, Vec<f64>)>,
    time_base_ms: f32,
    show_peaks: bool,
}

impl Default for AnalyzerPage {
    fn default() -> Self {
        Self::new()
    }
}

/// Index of the first rising zero crossing that leaves `window` samples
/// after it, searching the older part of the buffer so the trace is stable.
pub fn trigger_index(samples: &[f64], window: usize) -> usize {
    let last_start = samples.len().saturating_sub(window);
    let search_from = last_start.saturating_sub(window);
    (search_from..last_start)
        .rev()
        .find(|&i| i > 0 && samples[i - 1] < 0.0 && samples[i] >= 0.0)
        .unwrap_or(last_start)
}

impl AnalyzerPage {
    pub fn new() -> Self {
        Self {
            analyzer: SpectrumAnalyzer::new(4096),
            smoothed: Vec::new(),
            peak_hold: Vec::new(),
            frozen: None,
            time_base_ms: 20.0,
            show_peaks: true,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, buffer: &VecDeque<f64>) {
        let palette = Palette::of(ui.ctx());
        let rate = GLOBAL_CONFIG.sample_rate as f64;
        ui.horizontal_wrapped(|ui| {
            ui.label("Time base:");
            for ms in TIME_BASES_MS {
                ui.selectable_value(&mut self.time_base_ms, ms, format!("{ms:.0} ms"));
            }
            ui.separator();
            let mut frozen = self.frozen.is_some();
            if ui.toggle_value(&mut frozen, "❄ Freeze").on_hover_text("Hold the current picture").changed() {
                self.frozen = None;
                if frozen {
                    let wave: Vec<f64> = buffer.iter().copied().collect();
                    self.frozen = Some((wave, self.smoothed.clone()));
                }
            }
            ui.checkbox(&mut self.show_peaks, "Peak hold");
            if ui.button("Reset peaks").clicked() {
                self.peak_hold.clear();
            }
        });

        let (wave, spectrum) = match &self.frozen {
            Some((w, s)) => (w.clone(), s.clone()),
            None => {
                let db = self.analyzer.db(buffer);
                if self.smoothed.len() != db.len() {
                    self.smoothed = db.clone();
                }
                for (s, d) in self.smoothed.iter_mut().zip(&db) {
                    // Fast attack, slower release, like an analog analyzer.
                    *s = if *d > *s { *d } else { *s * 0.8 + *d * 0.2 };
                }
                if self.peak_hold.len() != db.len() {
                    self.peak_hold = vec![-200.0; db.len()];
                }
                for (p, d) in self.peak_hold.iter_mut().zip(&self.smoothed) {
                    *p = p.max(*d);
                }
                (buffer.iter().copied().collect(), self.smoothed.clone())
            }
        };

        let height = ((ui.available_height() - 40.0) / 2.0).max(120.0);
        ui.label(RichText::new("Waveform").color(palette.text_dim));
        let window = ((self.time_base_ms / 1000.0) as f64 * rate) as usize;
        let start = trigger_index(&wave, window);
        let points: PlotPoints = wave[start..(start + window).min(wave.len())]
            .iter()
            .enumerate()
            .map(|(i, &y)| [i as f64 / rate * 1000.0, y])
            .collect();
        Plot::new("analyzer_waveform")
            .height(height)
            .default_x_bounds(0.0, self.time_base_ms as f64)
            .default_y_bounds(-1.0, 1.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .x_axis_formatter(|mark, _| format!("{:.0} ms", mark.value))
            .show(ui, |plot_ui| {
                plot_ui.line(Line::new("wave", points).color(palette.accent).width(1.5_f32));
            });

        ui.label(RichText::new("Spectrum (dBFS)").color(palette.text_dim));
        let bin_hz = rate / self.analyzer.size() as f64;
        let to_points = |values: &[f64]| -> PlotPoints<'static> {
            values
                .iter()
                .enumerate()
                .skip(1)
                .map(|(i, &db)| [(i as f64 * bin_hz).log10(), db.max(-120.0)])
                .filter(|[x, _]| *x >= 20f64.log10() && *x <= 20_000f64.log10())
                .collect()
        };
        let spectrum_points = to_points(&spectrum);
        let peak_points = (self.show_peaks && self.frozen.is_none()).then(|| to_points(&self.peak_hold));
        Plot::new("analyzer_spectrum")
            .height(height)
            .default_x_bounds(20f64.log10(), 20_000f64.log10())
            .default_y_bounds(-100.0, 0.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .x_grid_spacer(log_grid_spacer(10))
            .x_axis_formatter(|mark: GridMark, _| {
                let hz = 10f64.powf(mark.value);
                if hz >= 1000.0 { format!("{:.0}k", hz / 1000.0) } else { format!("{hz:.0}") }
            })
            .y_axis_formatter(|mark, _| format!("{:.0} dB", mark.value))
            .label_formatter(|_, p| format!("{:.0} Hz, {:.1} dB", 10f64.powf(p.x), p.y))
            .show(ui, |plot_ui| {
                if let Some(peaks) = peak_points {
                    plot_ui.line(Line::new("peak", peaks).color(palette.text_dim.gamma_multiply(0.6)).width(1.0_f32));
                }
                plot_ui.line(Line::new("spectrum", spectrum_points).color(palette.accent).width(1.5_f32).fill(-120.0_f32));
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    #[test]
    fn trigger_finds_a_rising_zero_crossing() {
        let samples: Vec<f64> = (0..2000).map(|i| (i as f64 * 0.05).sin()).collect();
        let i = trigger_index(&samples, 500);
        assert!(i + 500 <= samples.len());
        assert!(samples[i - 1] < 0.0 && samples[i] >= 0.0);
        // Too little data for a full window: start at 0.
        assert_eq!(trigger_index(&[0.1, 0.2], 10), 0);
    }

    #[test]
    fn page_renders_live_and_frozen() {
        let ctx = egui::Context::default();
        let mut page = AnalyzerPage::new();
        let buffer: VecDeque<f64> = (0..4096).map(|i| (i as f64 * 0.03).sin() * 0.5).collect();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| page.ui(ui, &buffer));
        });
        page.frozen = Some((buffer.iter().copied().collect(), page.smoothed.clone()));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| page.ui(ui, &VecDeque::new()));
        });
        assert!(!page.peak_hold.is_empty());
    }
}
