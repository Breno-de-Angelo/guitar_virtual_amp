//! "My guitar" row of the Transcribe form: measures the player's guitar
//! once (`crate::tone::calibrate`) so tone estimates are fitted for it.
//!
//! The player either records about a minute of playing in the app (the
//! analysis stream, taken before the pedals: `AudioControls::analysis_dry`
//! is turned on for it) or picks a DI recording. A worker thread
//! transcribes the notes and measures the colour; the result goes to
//! `AppSettings::tone_guitar`. In the benchmark this lowered the tone error
//! by 0.25-0.56 dB on clean targets (`docs/papers/tone-estimation/`,
//! Section 14).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, unbounded};
use eframe::egui::{self, Color32, RichText, Ui};

use crate::shared::i18n::tr;
use crate::shared::settings::{AppSettings, save_settings};
use crate::tone::{GuitarCalibration, MIN_CALIBRATION_SECS};
use crate::trf;

/// Recommended length of the recording.
const TARGET_SECS: f64 = 60.0;
/// The recording stops by itself after this.
const MAX_SECS: f64 = 120.0;
/// Widest the help text gets before wrapping.
const MAX_TEXT_WIDTH: f32 = 640.0;
/// No samples for this long while recording: audio isn't running.
const NO_SIGNAL_SECS: f64 = 2.0;

enum Msg {
    Progress(f32),
    Done(Result<GuitarCalibration, String>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: f32,
}

struct Take {
    samples: Vec<f32>,
    rate: u32,
    started: Instant,
    /// RMS of the latest samples, for the level readout.
    level: f32,
}

impl Take {
    fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.rate.max(1) as f64
    }
}

#[derive(Default)]
pub struct GuitarCalibrationPanel {
    expanded: bool,
    take: Option<Take>,
    job: Option<Job>,
    error: Option<String>,
}

impl GuitarCalibrationPanel {
    pub fn is_busy(&self) -> bool {
        self.take.is_some() || self.job.is_some()
    }

    /// Guitar samples (the analysis stream, at `rate`) since the last frame.
    /// Kept only while recording.
    pub fn feed_input(&mut self, samples: &[f32], rate: u32) {
        let Some(take) = &mut self.take else { return };
        take.rate = rate;
        take.samples.extend_from_slice(samples);
        if !samples.is_empty() {
            take.level = (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt();
        }
    }

    fn start_recording(&mut self, settings: &mut AppSettings) {
        // The recording must be the guitar before the pedals.
        if !settings.ui.analysis_dry {
            settings.ui.analysis_dry = true;
            save_settings(settings);
        }
        self.error = None;
        self.take = Some(Take {
            samples: Vec::new(),
            rate: crate::shared::config::GLOBAL_CONFIG.sample_rate as u32,
            started: Instant::now(),
            level: 0.0,
        });
    }

    fn measure(&mut self, samples: Vec<f32>, rate: u32, settings: &AppSettings) {
        let model = settings.transcription_guitar_model;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let mut report = |p: f32| {
                let _ = progress_tx.send(Msg::Progress(p));
            };
            let result = crate::transcribe::calibrate_guitar(&samples, rate, model, &mut report, &flag);
            let _ = tx.send(Msg::Done(result));
        });
        self.error = None;
        self.job = Some(Job { rx, cancel, progress: 0.0 });
    }

    #[cfg(not(target_os = "android"))]
    fn measure_file(&mut self, path: &std::path::Path, settings: &AppSettings) {
        match crate::backend::audio_file::decode_file(path) {
            Ok(audio) => self.measure(audio.samples, audio.sample_rate, settings),
            Err(e) => self.error = Some(e),
        }
    }

    /// Picks up the worker's result.
    fn poll(&mut self, settings: &mut AppSettings) {
        let Some(job) = &mut self.job else { return };
        let mut done = None;
        for msg in job.rx.try_iter() {
            match msg {
                Msg::Progress(p) => job.progress = p,
                Msg::Done(result) => done = Some(result),
            }
        }
        match done {
            Some(Ok(calibration)) => {
                settings.tone_guitar = Some(calibration);
                save_settings(settings);
                self.job = None;
                self.expanded = false;
            }
            Some(Err(e)) => {
                self.job = None;
                // A cancelled job says so; nothing to report.
                if !e.contains("cancel") {
                    self.error = Some(e);
                }
            }
            None => {}
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        self.poll(settings);
        if self.is_busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("🎸 My guitar:", "🎸 Minha guitarra:"));
            match &settings.tone_guitar {
                Some(c) => {
                    ui.label(RichText::new(trf!(
                        "calibrated ({:.0} s, {} notes)",
                        "calibrada ({:.0} s, {} notas)",
                        c.seconds,
                        c.notes
                    )))
                    .on_hover_text(tr(
                        "Estimated tones are fitted for the guitar you recorded.",
                        "Os timbres estimados são ajustados para a guitarra que você gravou.",
                    ));
                }
                None => {
                    ui.label(RichText::new(tr("not calibrated", "não calibrada")).weak()).on_hover_text(tr(
                        "Estimated tones are fitted for an average guitar. Calibrating takes a minute of playing \
                         and fits them for yours.",
                        "Os timbres estimados são ajustados para uma guitarra média. Calibrar leva um minuto \
                         tocando e ajusta para a sua.",
                    ));
                }
            }
            if !self.expanded && !self.is_busy() {
                let label = if settings.tone_guitar.is_some() {
                    tr("Recalibrate...", "Calibrar de novo...")
                } else {
                    tr("Calibrate...", "Calibrar...")
                };
                if ui.small_button(label).clicked() {
                    self.expanded = true;
                    self.error = None;
                }
                if settings.tone_guitar.is_some()
                    && ui
                        .small_button(tr("Forget", "Esquecer"))
                        .on_hover_text(tr("Fit tones for an average guitar again.", "Voltar a ajustar os timbres para uma guitarra média."))
                        .clicked()
                {
                    settings.tone_guitar = None;
                    save_settings(settings);
                }
            }
        });
        if self.expanded || self.is_busy() {
            ui.indent("guitar_calibration", |ui| {
                // Long help text wraps instead of widening the form.
                ui.set_max_width(ui.available_width().min(MAX_TEXT_WIDTH));
                self.expanded_ui(ui, settings)
            });
        }
        if let Some(e) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
    }

    fn expanded_ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                ui.label(tr("Measuring your guitar...", "Medindo sua guitarra..."));
                ui.add(egui::ProgressBar::new(job.progress).desired_width(160.0).show_percentage());
                if ui.small_button(tr("Cancel", "Cancelar")).clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
            return;
        }
        if let Some(take) = &self.take {
            let secs = take.seconds();
            let waited = take.started.elapsed().as_secs_f64();
            let mut stop = false;
            let mut cancel = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("⏺ {}:{:02}", secs as u64 / 60, secs as u64 % 60)).color(Color32::LIGHT_RED),
                );
                let db = 20.0 * take.level.max(1e-6).log10();
                ui.label(RichText::new(format!("{db:.0} dBFS")).small().weak());
                let enough = secs >= MIN_CALIBRATION_SECS;
                if ui
                    .add_enabled(enough, egui::Button::new(tr("⏹ Stop and measure", "⏹ Parar e medir")))
                    .on_disabled_hover_text(trf!(
                        "Play for at least {MIN_CALIBRATION_SECS:.0} s (a minute is better).",
                        "Toque pelo menos {MIN_CALIBRATION_SECS:.0} s (um minuto é melhor)."
                    ))
                    .clicked()
                {
                    stop = true;
                }
                if ui.small_button(tr("Cancel", "Cancelar")).clicked() {
                    cancel = true;
                }
            });
            if secs == 0.0 && waited > NO_SIGNAL_SECS {
                ui.colored_label(
                    Color32::YELLOW,
                    tr(
                        "No signal: is the audio running and the guitar's input selected?",
                        "Sem sinal: o áudio está ligado e a entrada da guitarra escolhida?",
                    ),
                );
            } else if secs < TARGET_SECS {
                ui.label(
                    RichText::new(trf!(
                        "Keep playing: about {:.0} s to go.",
                        "Continue tocando: faltam uns {:.0} s.",
                        TARGET_SECS - secs
                    ))
                    .small()
                    .weak(),
                );
            } else {
                ui.label(RichText::new(tr("That's enough: stop when you like.", "Já basta: pare quando quiser.")).small().weak());
            }
            if secs >= MAX_SECS {
                stop = true;
            }
            if cancel {
                self.take = None;
            } else if stop && let Some(take) = self.take.take() {
                self.measure(take.samples, take.rate, settings);
            }
            return;
        }
        ui.add(egui::Label::new(
            RichText::new(tr(
                "Play about a minute on the guitar you will use: single notes and chords, low and high on the \
                 neck, with your usual pickup. The app records the guitar before the pedals, so the preset on \
                 the pedalboard doesn't matter.",
                "Toque cerca de um minuto na guitarra que você vai usar: notas soltas e acordes, graves e \
                 agudos no braço, com o captador de sempre. O app grava a guitarra antes dos pedais, então o \
                 preset na pedaleira não importa.",
            ))
            .small(),
        ).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("⏺ Record", "⏺ Gravar")).clicked() {
                self.start_recording(settings);
            }
            #[cfg(not(target_os = "android"))]
            if ui
                .button(tr("📂 Use a DI recording...", "📂 Usar uma gravação DI..."))
                .on_hover_text(tr(
                    "A recording of the guitar straight from the interface, without effects or amp (15 s or more).",
                    "Uma gravação da guitarra direto da interface, sem efeitos nem amplificador (15 s ou mais).",
                ))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(tr("Audio", "Áudio"), crate::backend::audio_file::AUDIO_EXTENSIONS)
                    .pick_file()
            {
                self.measure_file(&path, settings);
            }
            if ui.small_button(tr("Close", "Fechar")).clicked() {
                self.expanded = false;
                self.error = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_kept_only_while_recording() {
        let mut panel = GuitarCalibrationPanel::default();
        panel.feed_input(&[0.5; 100], 48_000);
        assert!(!panel.is_busy());
        let mut settings = AppSettings::default();
        settings.ui.analysis_dry = false;
        panel.start_recording(&mut settings);
        assert!(settings.ui.analysis_dry, "records the guitar before the pedals");
        panel.feed_input(&[0.5; 480], 48_000);
        panel.feed_input(&[0.5; 480], 48_000);
        let take = panel.take.as_ref().unwrap();
        assert_eq!(take.samples.len(), 960);
        assert!((take.seconds() - 0.02).abs() < 1e-9);
        assert!((take.level - 0.5).abs() < 1e-6);
    }
}
