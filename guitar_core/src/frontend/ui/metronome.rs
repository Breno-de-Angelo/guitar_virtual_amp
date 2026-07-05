use std::time::{Duration, Instant};

use eframe::egui::{self, Ui};

use crate::frontend::lib::metronome::Metronome;
use crate::shared::pedals::PedalDescription;

/// UI wrapper around `Metronome`: BPM slider, tap-tempo/start-stop buttons, a
/// flashing beat indicator, and a "sync" button that writes the current BPM
/// into any Delay/Tremolo pedals already in the chain.
pub struct MetronomePanel {
    metronome: Metronome,
    flash_until: Instant,
}

impl Default for MetronomePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl MetronomePanel {
    pub fn new() -> Self {
        Self {
            metronome: Metronome::new(),
            flash_until: Instant::now(),
        }
    }

    /// Renders the panel. Returns `true` if it wrote new tempo-synced values
    /// into `pedal_chain`, so the caller knows to push a chain update.
    pub fn ui(&mut self, ui: &mut Ui, pedal_chain: &mut [PedalDescription]) -> bool {
        ui.horizontal(|ui| {
            ui.label("BPM:");
            ui.add(egui::Slider::new(&mut self.metronome.bpm, 30.0..=300.0));
        });

        ui.horizontal(|ui| {
            if ui.button("Tap Tempo").clicked() {
                self.metronome.tap();
            }

            let label = if self.metronome.running {
                "Stop"
            } else {
                "Start"
            };
            if ui.button(label).clicked() {
                if self.metronome.running {
                    self.metronome.stop();
                } else {
                    self.metronome.start();
                }
            }

            if self.metronome.tick() {
                self.flash_until = Instant::now() + Duration::from_millis(80);
            }
            let flashing = Instant::now() < self.flash_until;
            let color = if flashing {
                egui::Color32::from_rgb(255, 200, 0)
            } else {
                egui::Color32::DARK_GRAY
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 8.0, color);
        });

        let mut updated = false;
        if ui
            .button("Sync tempo-synced pedals to BPM")
            .on_hover_text("Sets Delay time / Tremolo rate to match the current BPM")
            .clicked()
        {
            for pedal in pedal_chain.iter_mut() {
                match pedal {
                    PedalDescription::Delay(params) => {
                        params.delay_ms = 60_000.0 / self.metronome.bpm;
                        updated = true;
                    }
                    PedalDescription::Tremolo(params) => {
                        params.rate_hz = self.metronome.bpm / 60.0;
                        updated = true;
                    }
                    _ => {}
                }
            }
        }

        updated
    }
}
