use std::collections::VecDeque;

use eframe::egui::{self, Ui};

use crate::frontend::lib::pitch::{detect_pitch, freq_to_note};
use crate::shared::config::GLOBAL_CONFIG;

/// Renders the tuner panel: detected frequency, nearest note, and a cents-off
/// meter, driven off the same sample buffer as the oscilloscope/FFT view.
pub fn tuner_ui(ui: &mut Ui, buffer: &VecDeque<f64>) {
    match detect_pitch(buffer, GLOBAL_CONFIG.sample_rate) {
        Some(freq) => {
            let (name, octave, cents) = freq_to_note(freq);
            ui.label(format!("{freq:.1} Hz"));
            ui.label(egui::RichText::new(format!("{name}{octave}")).size(24.0));

            let in_tune = cents.abs() < 5.0;
            let color = if in_tune {
                egui::Color32::from_rgb(80, 220, 100)
            } else {
                egui::Color32::from_rgb(220, 160, 60)
            };
            // Meter spans -50..+50 cents, centered.
            let normalized = ((cents + 50.0) / 100.0).clamp(0.0, 1.0) as f32;
            ui.add(
                egui::ProgressBar::new(normalized)
                    .text(format!("{cents:+.0} cents"))
                    .fill(color),
            );
        }
        None => {
            ui.label("Play a note to tune...");
        }
    }
}
