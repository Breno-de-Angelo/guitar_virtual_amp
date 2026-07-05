use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Ui};

use crate::frontend::lib::metronome::Metronome;
use crate::shared::pedals::{PedalDescription, PedalInstance};

/// UI wrapper around `Metronome`: BPM slider, tap-tempo/start-stop buttons, a
/// flashing beat indicator, and a "sync" button that writes the current BPM
/// into any Delay/Tremolo pedals already in the chain.
///
/// Also mirrors `running`/`bpm` into a pair of atomics shared with the
/// realtime audio output callback (`backend::capture`), which is what
/// actually generates the audible click -- the UI-frame-driven `tick()`
/// below only drives the visual beat indicator, since egui's frame rate
/// isn't accurate enough for audio timing.
pub struct MetronomePanel {
    metronome: Metronome,
    flash_until: Instant,
    audio_running: Arc<AtomicBool>,
    audio_bpm: Arc<AtomicU32>,
}

impl MetronomePanel {
    pub fn new(audio_running: Arc<AtomicBool>, audio_bpm: Arc<AtomicU32>) -> Self {
        Self {
            metronome: Metronome::new(),
            flash_until: Instant::now(),
            audio_running,
            audio_bpm,
        }
    }

    /// Renders the panel. Returns `true` if it wrote new tempo-synced values
    /// into `pedal_chain`, so the caller knows to push a chain update.
    pub fn ui(&mut self, ui: &mut Ui, pedal_chain: &mut [PedalInstance]) -> bool {
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

        // Mirror running/bpm into the atomics the realtime output callback reads
        // to generate the audible click. Unconditional store each frame is cheap
        // and keeps this correct regardless of which control above changed them.
        self.audio_running
            .store(self.metronome.running, Ordering::Relaxed);
        self.audio_bpm
            .store(self.metronome.bpm.to_bits(), Ordering::Relaxed);

        let mut updated = false;
        if ui
            .button("Sync tempo-synced pedals to BPM")
            .on_hover_text("Sets Delay time / Tremolo rate to match the current BPM")
            .clicked()
        {
            for instance in pedal_chain.iter_mut() {
                match &mut instance.description {
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
