//! "Settings" page: audio devices and buffer, input/output levels, theme
//! and UI scale, MIDI controller bindings (with MIDI learn), recordings,
//! keyboard shortcuts and where the app keeps its files.

use eframe::egui::{self, RichText, Ui};

use crate::backend::midi::MidiInputManager;
use crate::backend::recorder::{Recording, list_recordings, recordings_dir};
use crate::frontend::devices::DeviceManager;
use crate::frontend::lib::latency::{LatencyProbe, ProbeState};
use crate::frontend::style::{self, Palette};
use crate::shared::midi::MidiAction;
use crate::shared::pedals::PedalInstance;
use crate::shared::settings::{AppSettings, ThemePref};

/// `(keys, what they do)`, shown here and in the F1 overlay.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+Z / Ctrl+Shift+Z (Ctrl+Y)", "Undo / redo pedal edits"),
    ("1 … 9", "Bypass / enable pedal 1-9"),
    ("PageUp / PageDown", "Previous / next preset (setlist)"),
    ("Ctrl+S", "Save the current preset"),
    ("M", "Mute / unmute the output"),
    ("T", "Tap tempo"),
    ("K", "Start / stop the metronome"),
    ("L", "Looper: record / play / overdub"),
    ("Ctrl+R", "Start / stop recording"),
    ("Space", "Play / pause the tab (Tabs page)"),
    ("Ctrl+1 … Ctrl+7", "Switch page"),
    ("F1", "Show these shortcuts"),
];

/// Opens a folder in the system file manager.
#[cfg(not(target_os = "android"))]
pub fn open_folder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

#[derive(Default)]
pub struct SettingsPage {
    /// The action waiting for a MIDI message to bind ("MIDI learn").
    pub learning: Option<MidiAction>,
    ports: Option<Result<Vec<String>, String>>,
    recordings: Option<Vec<Recording>>,
}

/// What the page changed.
#[derive(Default)]
pub struct SettingsOutput {
    /// Input gain / output volume moved: push them to the audio thread.
    pub levels_changed: bool,
    /// The dry/processed analysis choice changed.
    pub analysis_changed: bool,
    pub start_latency_probe: bool,
    pub cancel_latency_probe: bool,
    /// Practice mode's extra latency should become this (ms).
    pub use_extra_latency_ms: Option<f32>,
    /// Settings changed: save them (debounced by the caller).
    pub save: bool,
}

impl SettingsPage {
    /// Forget the cached recordings list (after a new recording).
    pub fn invalidate_recordings(&mut self) {
        self.recordings = None;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        devices: &mut DeviceManager,
        chain: &[PedalInstance],
        settings: &mut AppSettings,
        midi: &mut MidiInputManager,
        probe: &LatencyProbe,
        muted: bool,
    ) -> SettingsOutput {
        let palette = Palette::of(ui.ctx());
        let mut out = SettingsOutput::default();
        let mut save = false;
        egui::ScrollArea::vertical().id_salt("settings_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let section = |ui: &mut Ui, title: &str, body: &mut dyn FnMut(&mut Ui)| {
                style::card_frame(&palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    style::section_title(ui, title);
                    ui.add_space(style::space::XS);
                    body(ui);
                });
                ui.add_space(style::space::M);
            };

            section(ui, "🔊 Audio", &mut |ui| devices.ui(ui, chain, settings));

            section(ui, "⏱ Latency and analysis", &mut |ui| {
                ui.label(
                    RichText::new(
                        "Practice mode compensates the time sound takes to go out and come back in.                          Measure it: point the microphone at the speakers (or connect the output to the input                          with a cable), keep the guitar quiet, and press Measure.",
                    )
                    .small()
                    .color(palette.text_dim),
                );
                ui.horizontal_wrapped(|ui| {
                    if probe.running() {
                        ui.spinner();
                        if let ProbeState::Running { done, of } = probe.state() {
                            ui.label(format!("Listening for pulse {}/{of}…", (*done + 1).min(*of)));
                        }
                        if ui.button("Cancel").clicked() {
                            out.cancel_latency_probe = true;
                        }
                    } else {
                        let ready = devices.audio_running() && !muted;
                        if ui.add_enabled(ready, egui::Button::new("⏱ Measure latency")).clicked() {
                            out.start_latency_probe = true;
                        }
                        if !ready {
                            ui.label(RichText::new("(needs audio running and unmuted)").color(palette.text_dim));
                        }
                    }
                });
                match probe.state() {
                    ProbeState::Done { latency_secs, heard } => {
                        let measured_ms = (*latency_secs * 1000.0) as f32;
                        let reported_ms = devices.configured_latency_secs * 1000.0;
                        let extra = measured_ms - reported_ms;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("Round trip: {measured_ms:.1} ms")).strong().color(palette.ok));
                            ui.label(
                                RichText::new(format!(
                                    "(the driver reports {reported_ms:.1} ms, so {extra:+.1} ms extra; {heard} pulses heard)"
                                ))
                                .color(palette.text_dim),
                            );
                            if ui.button("Use for practice mode").clicked() {
                                out.use_extra_latency_ms = Some(extra);
                            }
                        });
                    }
                    ProbeState::Failed(why) => {
                        ui.colored_label(palette.warn, why);
                    }
                    _ => {}
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Practice extra latency: {:+.0} ms", settings.practice.extra_latency_ms));
                });
                ui.separator();
                if ui
                    .checkbox(&mut settings.ui.analysis_dry, "Tuner, practice and analyzer listen to the guitar before the pedals")
                    .on_hover_text("Distortion, octavers and delays make pitch detection harder; the dry signal is cleaner")
                    .changed()
                {
                    out.analysis_changed = true;
                    save = true;
                }
            });

            section(ui, "🔉 Levels", &mut |ui| {
                let prefs = &mut settings.ui;
                ui.horizontal_wrapped(|ui| {
                    out.levels_changed |= ui
                        .add(egui::Slider::new(&mut prefs.input_gain_db, -24.0..=24.0).suffix(" dB").text("input gain"))
                        .on_hover_text("Boost a weak pickup or tame a hot one before the pedals")
                        .changed();
                    if ui.small_button("0 dB").clicked() {
                        prefs.input_gain_db = 0.0;
                        out.levels_changed = true;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    out.levels_changed |= ui
                        .add(egui::Slider::new(&mut prefs.output_volume_db, -60.0..=12.0).suffix(" dB").text("master volume"))
                        .changed();
                    if ui.small_button("0 dB").clicked() {
                        prefs.output_volume_db = 0.0;
                        out.levels_changed = true;
                    }
                });
                save |= out.levels_changed;
            });

            section(ui, "🎨 Appearance", &mut |ui| {
                let prefs = &mut settings.ui;
                ui.horizontal_wrapped(|ui| {
                    ui.label("Theme:");
                    for (theme, label) in [(ThemePref::Dark, "Dark"), (ThemePref::Light, "Light"), (ThemePref::System, "System")] {
                        save |= ui.selectable_value(&mut prefs.theme, theme, label).changed();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("UI scale:");
                    for zoom in [0.8, 1.0, 1.25, 1.5, 2.0] {
                        save |= ui
                            .selectable_value(&mut prefs.zoom, zoom, format!("{:.0}%", zoom * 100.0))
                            .changed();
                    }
                    ui.label(RichText::new("(Ctrl + / Ctrl - also zoom)").small().color(palette.text_dim));
                });
            });

            section(ui, "🎹 MIDI controller", &mut |ui| {
                save |= self.midi_ui(ui, settings, chain, midi);
            });

            section(ui, "⏺ Recordings", &mut |ui| self.recordings_ui(ui));

            section(ui, "⌨ Keyboard shortcuts", &mut |ui| shortcuts_grid(ui));

            section(ui, "📁 Files", &mut |ui| {
                let rows: Vec<(&str, Option<std::path::PathBuf>)> = vec![
                    ("Presets", crate::shared::preset::user_preset_dir()),
                    ("Settings", crate::shared::settings::settings_path()),
                    ("App data (downloads, models, tabs)", crate::shared::paths::app_data_dir()),
                ];
                egui::Grid::new("files_grid").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                    for (label, path) in rows {
                        ui.label(label);
                        match path {
                            Some(p) => {
                                ui.add(egui::Label::new(RichText::new(p.display().to_string()).monospace().small()).truncate());
                                #[cfg(not(target_os = "android"))]
                                {
                                    let folder = if p.extension().is_some() { p.parent().map(std::path::Path::to_path_buf) } else { Some(p.clone()) };
                                    if let Some(folder) = folder
                                        && ui.small_button("Open").clicked()
                                    {
                                        open_folder(&folder);
                                    }
                                }
                            }
                            None => {
                                ui.label(RichText::new("not available on this platform").color(palette.text_dim));
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.label(
                    RichText::new(format!("Guitar Virtual Amp {} · GPL-3.0", env!("CARGO_PKG_VERSION")))
                        .small()
                        .color(palette.text_dim),
                );
            });
        });
        out.save |= save;
        out
    }

    fn midi_ui(
        &mut self,
        ui: &mut Ui,
        settings: &mut AppSettings,
        chain: &[PedalInstance],
        midi: &mut MidiInputManager,
    ) -> bool {
        let palette = Palette::of(ui.ctx());
        let mut changed = false;
        if self.ports.is_none() {
            self.ports = Some(MidiInputManager::list_ports());
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Input port:");
            let current = midi.port().unwrap_or("(none)").to_string();
            let mut picked: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("midi_port").selected_text(current).width(260.0).show_ui(ui, |ui| {
                if ui.selectable_label(midi.port().is_none(), "(none)").clicked() {
                    picked = Some(None);
                }
                if let Some(Ok(ports)) = &self.ports {
                    for port in ports {
                        if ui.selectable_label(midi.port() == Some(port.as_str()), port).clicked() {
                            picked = Some(Some(port.clone()));
                        }
                    }
                }
            });
            if ui.button("🔄").on_hover_text("Look for MIDI devices again").clicked() {
                self.ports = None;
            }
            match picked {
                Some(None) => {
                    midi.disconnect();
                    settings.ui.midi.input_port = None;
                    changed = true;
                }
                Some(Some(port)) if midi.connect(&port).is_ok() => {
                    settings.ui.midi.input_port = Some(port);
                    changed = true;
                }
                _ => {}
            }
        });
        match &self.ports {
            Some(Err(e)) => {
                ui.colored_label(palette.warn, format!("MIDI unavailable: {e}"));
            }
            Some(Ok(ports)) if ports.is_empty() => {
                ui.label(RichText::new("No MIDI inputs found. Plug in a controller and press 🔄.").color(palette.text_dim));
            }
            _ => {}
        }
        if let Some(error) = midi.error() {
            ui.colored_label(palette.danger, error);
        }
        ui.label(
            RichText::new(
                "Press Learn, then move a knob, press a footswitch or send a program change. \
                 Footswitch CCs toggle on press (value ≥ 64).",
            )
            .small()
            .color(palette.text_dim),
        );
        let mut actions: Vec<MidiAction> = MidiAction::GLOBAL.to_vec();
        actions.extend((0..chain.len().min(16)).map(MidiAction::TogglePedal));
        egui::Grid::new("midi_bindings").num_columns(3).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
            for action in actions {
                let label = match action {
                    MidiAction::TogglePedal(i) => format!("Toggle pedal {} ({})", i + 1, chain[i].description.name()),
                    other => other.label(),
                };
                ui.label(label);
                let learning = self.learning == Some(action);
                match settings.ui.midi.trigger_for(action) {
                    _ if learning => {
                        ui.colored_label(palette.accent, "waiting for MIDI…");
                    }
                    Some(trigger) => {
                        ui.label(RichText::new(trigger.label()).monospace());
                    }
                    None => {
                        ui.label(RichText::new("—").color(palette.text_dim));
                    }
                }
                ui.horizontal(|ui| {
                    if learning {
                        if ui.small_button("Cancel").clicked() {
                            self.learning = None;
                        }
                    } else if ui.small_button("Learn").clicked() {
                        self.learning = Some(action);
                    }
                    if settings.ui.midi.trigger_for(action).is_some() && ui.small_button("✖").on_hover_text("Unbind").clicked() {
                        settings.ui.midi.unbind(action);
                        changed = true;
                    }
                });
                ui.end_row();
            }
        });
        changed
    }

    fn recordings_ui(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui.ctx());
        let Some(dir) = recordings_dir() else {
            ui.label(RichText::new("Recording needs a data folder, which this platform doesn't have.").color(palette.text_dim));
            return;
        };
        ui.label(
            RichText::new("Press ⏺ in the top bar (or Ctrl+R) to record. Takes are 16-bit mono WAV files.")
                .small()
                .color(palette.text_dim),
        );
        ui.horizontal(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button("📂 Open folder").clicked() {
                open_folder(&dir);
            }
            if ui.button("🔄").on_hover_text("Refresh").clicked() {
                self.recordings = None;
            }
        });
        let recordings = self.recordings.get_or_insert_with(|| list_recordings(&dir));
        if recordings.is_empty() {
            ui.label(RichText::new("No recordings yet.").color(palette.text_dim));
        }
        for recording in recordings.iter().take(10) {
            ui.horizontal(|ui| {
                let name = recording.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).monospace());
                let secs = recording.duration_secs;
                ui.label(RichText::new(format!("{}:{:04.1}", (secs / 60.0) as u64, secs % 60.0)).color(palette.text_dim));
            });
        }
    }
}

pub fn shortcuts_grid(ui: &mut Ui) {
    let palette = Palette::of(ui.ctx());
    egui::Grid::new(ui.id().with("shortcuts")).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for (keys, what) in SHORTCUTS {
            ui.label(RichText::new(*keys).monospace().color(palette.accent));
            ui.label(*what);
            ui.end_row();
        }
    });
}
