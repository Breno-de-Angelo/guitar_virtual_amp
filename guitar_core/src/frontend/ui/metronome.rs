use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eframe::egui::{self, RichText, Sense, Ui, vec2};

use crate::backend::controls::AudioControls;
use crate::frontend::lib::metronome::Metronome;
use crate::frontend::style::{self, Palette};
use crate::shared::pedals::{PedalDescription, PedalInstance};
use crate::shared::settings::UiPrefs;

/// UI wrapper around `Metronome`: big BPM readout, tap tempo, start/stop,
/// beats per bar (the first beat is accented), click volume, beat lights,
/// and a "sync" button that writes the BPM into Delay/Tremolo pedals.
///
/// Mirrors `running`/`bpm` into atomics shared with the realtime output
/// callback (`backend::capture`), which generates the audible click. The
/// beat lights follow the clicks the audio thread reports
/// (`AudioControls::metronome_position`), so they flash with what you hear.
pub struct MetronomePanel {
    metronome: Metronome,
    audio_running: Arc<AtomicBool>,
    audio_bpm: Arc<AtomicU32>,
    controls: Arc<AudioControls>,
    last_clicks: u32,
    flash: f32,
}

/// Classic tempo markings, for the label under the BPM.
fn tempo_name(bpm: f32) -> &'static str {
    match bpm as u32 {
        0..=59 => "Largo",
        60..=75 => "Adagio",
        76..=107 => "Andante",
        108..=119 => "Moderato",
        120..=155 => "Allegro",
        156..=175 => "Vivace",
        _ => "Presto",
    }
}

impl MetronomePanel {
    pub fn new(audio_running: Arc<AtomicBool>, audio_bpm: Arc<AtomicU32>, controls: Arc<AudioControls>) -> Self {
        Self { metronome: Metronome::new(), audio_running, audio_bpm, controls, last_clicks: 0, flash: 0.0 }
    }

    /// Loads the persisted tempo, accent and volume.
    pub fn apply_prefs(&mut self, prefs: &UiPrefs) {
        self.metronome.bpm = prefs.metronome_bpm.clamp(30.0, 300.0);
        self.controls.set_metronome_beats_per_bar(prefs.metronome_beats_per_bar);
        self.controls.set_metronome_volume(prefs.metronome_volume);
        self.sync_audio();
    }

    fn sync_audio(&self) {
        self.audio_running.store(self.metronome.running, Ordering::Relaxed);
        self.audio_bpm.store(self.metronome.bpm.to_bits(), Ordering::Relaxed);
    }

    pub fn running(&self) -> bool {
        self.metronome.running
    }

    pub fn toggle(&mut self) {
        if self.metronome.running {
            self.metronome.stop();
        } else {
            self.metronome.start();
        }
        self.sync_audio();
    }

    pub fn tap(&mut self, prefs: &mut UiPrefs) {
        self.metronome.tap();
        prefs.metronome_bpm = self.metronome.bpm;
        self.sync_audio();
    }

    /// A small "● 120" indicator for the top bar: flashes on every click.
    pub fn compact_indicator(&mut self, ui: &mut Ui) -> egui::Response {
        let palette = Palette::of(ui.ctx());
        self.poll_clicks(ui.ctx());
        let color = if self.metronome.running {
            palette.accent.gamma_multiply(0.35 + 0.65 * self.flash)
        } else {
            palette.text_dim.gamma_multiply(0.5)
        };
        ui.label(RichText::new(format!("⚫ {:.0}", self.metronome.bpm)).color(color))
    }

    fn poll_clicks(&mut self, ctx: &egui::Context) {
        let (_, clicks) = self.controls.metronome_position();
        if clicks != self.last_clicks {
            self.last_clicks = clicks;
            self.flash = 1.0;
        } else {
            self.flash = (self.flash - ctx.input(|i| i.stable_dt) * 6.0).max(0.0);
        }
    }

    /// Renders the panel. Returns `(pedal chain updated, prefs changed)`.
    pub fn ui(&mut self, ui: &mut Ui, pedal_chain: &mut [PedalInstance], prefs: &mut UiPrefs) -> (bool, bool) {
        let palette = Palette::of(ui.ctx());
        self.poll_clicks(ui.ctx());
        let mut prefs_changed = false;
        let before = self.metronome.bpm;

        ui.vertical_centered(|ui| {
            ui.label(RichText::new(format!("{:.0}", self.metronome.bpm)).size(style::text::DISPLAY).strong());
            ui.label(RichText::new(format!("BPM · {}", tempo_name(self.metronome.bpm))).color(palette.text_dim));
        });
        ui.horizontal(|ui| {
            let width = 360.0;
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            for (label, delta) in [("−5", -5.0), ("−1", -1.0)] {
                if ui.button(label).clicked() {
                    self.metronome.bpm += delta;
                }
            }
            ui.add(egui::Slider::new(&mut self.metronome.bpm, 30.0..=300.0).show_value(false));
            for (label, delta) in [("+1", 1.0), ("+5", 5.0)] {
                if ui.button(label).clicked() {
                    self.metronome.bpm += delta;
                }
            }
        });
        self.metronome.bpm = self.metronome.bpm.clamp(30.0, 300.0).round();

        // Beat lights.
        let beats = prefs.metronome_beats_per_bar.max(1);
        let (current, _) = self.controls.metronome_position();
        ui.horizontal(|ui| {
            let size = 22.0;
            let width = beats as f32 * (size + 8.0);
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            for b in 0..beats {
                let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
                let lit = self.metronome.running && b == current % beats;
                let base = if b == 0 && beats > 1 { palette.accent } else { palette.ok };
                let color = if lit { base.gamma_multiply(0.4 + 0.6 * self.flash.max(0.3)) } else { palette.control };
                ui.painter().circle_filled(rect.center(), size / 2.0 - 1.0, color);
            }
        });
        ui.add_space(style::space::S);

        ui.horizontal_wrapped(|ui| {
            let label = if self.metronome.running { "⏹ Stop" } else { "▶ Start" };
            if ui.add(egui::Button::new(RichText::new(label).strong()).min_size(vec2(90.0, 0.0))).clicked() {
                self.toggle();
            }
            if ui.button("👆 Tap").on_hover_text("Tap a few times in tempo (key: T)").clicked() {
                self.metronome.tap();
            }
            ui.separator();
            ui.label("Beats per bar:");
            egui::ComboBox::from_id_salt("metronome_beats")
                .selected_text(if prefs.metronome_beats_per_bar <= 1 {
                    "no accent".to_string()
                } else {
                    prefs.metronome_beats_per_bar.to_string()
                })
                .width(90.0)
                .show_ui(ui, |ui| {
                    prefs_changed |= ui.selectable_value(&mut prefs.metronome_beats_per_bar, 0, "no accent").changed();
                    for n in 2..=12 {
                        prefs_changed |= ui.selectable_value(&mut prefs.metronome_beats_per_bar, n, n.to_string()).changed();
                    }
                });
            ui.separator();
            prefs_changed |= ui
                .add(egui::Slider::new(&mut prefs.metronome_volume, 0.0..=2.0).text("click volume").fixed_decimals(1))
                .changed();
        });
        if prefs_changed {
            self.controls.set_metronome_beats_per_bar(prefs.metronome_beats_per_bar);
            self.controls.set_metronome_volume(prefs.metronome_volume);
        }
        if self.metronome.bpm != before || (prefs.metronome_bpm - self.metronome.bpm).abs() > 0.01 {
            prefs.metronome_bpm = self.metronome.bpm;
            prefs_changed = true;
        }

        // Mirror running/bpm into the atomics the realtime output callback reads
        // to generate the audible click.
        self.sync_audio();

        let mut updated = false;
        ui.add_space(style::space::XS);
        ui.horizontal_wrapped(|ui| {
            let quarter_ms = 60_000.0 / self.metronome.bpm;
            for (label, factor) in [("♩", 1.0), ("♪", 0.5), ("♪.", 0.75), ("♪³", 1.0 / 3.0)] {
                if ui
                    .button(format!("Sync delays to {label}"))
                    .on_hover_text(format!(
                        "Delay time {:.0} ms, tremolo rate in the same note value",
                        quarter_ms * factor
                    ))
                    .clicked()
                {
                    updated |= sync_pedals(pedal_chain, self.metronome.bpm, factor);
                }
            }
        });
        (updated, prefs_changed)
    }
}

/// Sets Delay times and Tremolo rates to `note` (in quarter notes) at `bpm`.
/// Returns whether any pedal changed.
pub fn sync_pedals(pedal_chain: &mut [PedalInstance], bpm: f32, note: f32) -> bool {
    let mut updated = false;
    for instance in pedal_chain.iter_mut() {
        match &mut instance.description {
            PedalDescription::Delay(params) => {
                params.delay_ms = (60_000.0 / bpm * note).clamp(1.0, 2000.0);
                updated = true;
            }
            PedalDescription::Tremolo(params) => {
                params.rate_hz = (bpm / 60.0 / note).clamp(0.5, 10.0);
                updated = true;
            }
            _ => {}
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::pedals::{DelayParams, TremoloParams};

    #[test]
    fn syncs_delay_and_tremolo_to_note_values() {
        let mut chain = vec![
            PedalInstance::new(PedalDescription::Delay(DelayParams::default())),
            PedalInstance::new(PedalDescription::Tremolo(TremoloParams::default())),
        ];
        assert!(sync_pedals(&mut chain, 120.0, 0.75));
        match (&chain[0].description, &chain[1].description) {
            (PedalDescription::Delay(d), PedalDescription::Tremolo(t)) => {
                assert!((d.delay_ms - 375.0).abs() < 1e-3);
                assert!((t.rate_hz - 2.0 / 0.75).abs() < 1e-3);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn panel_renders_and_persists_tempo() {
        let ctx = egui::Context::default();
        let controls = Arc::new(AudioControls::default());
        let mut panel = MetronomePanel::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)), controls.clone());
        let mut prefs = UiPrefs { metronome_bpm: 90.0, metronome_beats_per_bar: 3, ..Default::default() };
        panel.apply_prefs(&prefs);
        assert_eq!(controls.metronome_beats_per_bar(), 3);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.ui(ui, &mut [], &mut prefs);
            });
        });
        assert_eq!(panel.metronome.bpm, 90.0);
        assert_eq!(f32::from_bits(panel.audio_bpm.load(Ordering::Relaxed)), 90.0);
        panel.toggle();
        assert!(panel.audio_running.load(Ordering::Relaxed));
    }
}
