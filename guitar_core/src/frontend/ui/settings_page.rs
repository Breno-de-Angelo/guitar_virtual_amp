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
use crate::shared::i18n::{LanguagePref, tr};
use crate::shared::settings::{AppSettings, ThemePref};
use crate::trf;

/// `(keys, what they do in English, in Portuguese)`, shown here and in the
/// F1 overlay.
pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("Ctrl+Z / Ctrl+Shift+Z (Ctrl+Y)", "Undo / redo pedal edits", "Desfazer / refazer edições dos pedais"),
    ("1 … 9", "Bypass / enable pedal 1-9", "Desligar / ligar o pedal 1-9"),
    ("PageUp / PageDown", "Previous / next preset (setlist)", "Preset anterior / próximo (setlist)"),
    ("Ctrl+S", "Save the current preset", "Salvar o preset atual"),
    ("M", "Mute / unmute the output", "Silenciar / reativar a saída"),
    ("T", "Tap tempo", "Marcar o tempo (tap)"),
    ("K", "Start / stop the metronome", "Iniciar / parar o metrônomo"),
    ("L", "Looper: record / play / overdub", "Looper: gravar / tocar / sobrepor"),
    ("Ctrl+R", "Start / stop recording", "Iniciar / parar a gravação"),
    ("Space", "Play / pause the tab (Tabs page)", "Tocar / pausar a tab (página Tabs)"),
    ("Ctrl+1 … Ctrl+7", "Switch page", "Trocar de página"),
    ("F1", "Show these shortcuts", "Mostrar estes atalhos"),
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

/// The page's sections, in order, for the index at the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Audio,
    Latency,
    Levels,
    Appearance,
    Midi,
    Recordings,
    Shortcuts,
    Files,
}

impl Section {
    const ALL: [Section; 8] = [
        Section::Audio,
        Section::Latency,
        Section::Levels,
        Section::Appearance,
        Section::Midi,
        Section::Recordings,
        Section::Shortcuts,
        Section::Files,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Audio => tr("🔊 Audio", "🔊 Áudio"),
            Section::Latency => tr("⏱ Latency and analysis", "⏱ Latência e análise"),
            Section::Levels => tr("🔉 Levels", "🔉 Níveis"),
            Section::Appearance => tr("🎨 Appearance", "🎨 Aparência"),
            Section::Midi => tr("🎹 MIDI controller", "🎹 Controlador MIDI"),
            Section::Recordings => tr("⏺ Recordings", "⏺ Gravações"),
            Section::Shortcuts => tr("⌨ Keyboard shortcuts", "⌨ Atalhos de teclado"),
            Section::Files => tr("📁 Files", "📁 Arquivos"),
        }
    }
}

/// One level slider row: label (fixed width, so the sliders line up),
/// slider with its value, and a reset-to-0-dB button. On narrow windows
/// the label goes on its own line. Returns whether the value changed.
fn level_row(
    ui: &mut Ui,
    label: &str,
    hover: Option<&str>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    label_width: f32,
    narrow: bool,
) -> bool {
    let mut changed = false;
    if narrow {
        ui.label(label);
    }
    ui.horizontal(|ui| {
        if !narrow {
            ui.allocate_ui_with_layout(
                egui::vec2(label_width, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(label_width);
                    ui.label(label);
                },
            );
        }
        if narrow {
            // Leave room for the value box and the reset button.
            ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(80.0);
        } else {
            ui.spacing_mut().slider_width = 220.0;
        }
        let response = ui.add(egui::Slider::new(value, range).suffix(" dB").fixed_decimals(1));
        let response = match hover {
            Some(text) => response.on_hover_text(text),
            None => response,
        };
        changed |= response.changed();
        let reset = ui
            .add_enabled(*value != 0.0, egui::Button::new("⟲ 0 dB"))
            .on_hover_text(tr("Reset to 0 dB", "Voltar a 0 dB"))
            .on_disabled_hover_text(tr("Already at 0 dB", "Já está em 0 dB"));
        if reset.clicked() {
            *value = 0.0;
            changed = true;
        }
    });
    changed
}

#[derive(Default)]
pub struct SettingsPage {
    /// The action waiting for a MIDI message to bind ("MIDI learn").
    pub learning: Option<MidiAction>,
    ports: Option<Result<Vec<String>, String>>,
    recordings: Option<Vec<Recording>>,
    /// The section the index was clicked for: scrolled to when it's drawn.
    jump_to: Option<Section>,
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
        let narrow = ui.ctx().screen_rect().width() < style::NARROW_WIDTH;
        // Section index, outside the scroll area so it stays put.
        ui.horizontal_wrapped(|ui| {
            for section in Section::ALL {
                if ui.button(section.title()).clicked() {
                    self.jump_to = Some(section);
                }
            }
        });
        ui.add_space(style::space::XS);
        let mut jump_to = self.jump_to.take();
        egui::ScrollArea::vertical().id_salt("settings_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut section = |ui: &mut Ui, which: Section, body: &mut dyn FnMut(&mut Ui)| {
                if jump_to == Some(which) {
                    ui.scroll_to_cursor(Some(egui::Align::TOP));
                    jump_to = None;
                }
                style::card_frame(&palette).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    style::section_title(ui, which.title());
                    ui.add_space(style::space::XS);
                    body(ui);
                });
                ui.add_space(style::space::M);
            };

            section(ui, Section::Audio, &mut |ui| devices.ui(ui, chain, settings));

            section(ui, Section::Latency, &mut |ui| {
                ui.label(
                    RichText::new(
                        tr(
                            "Practice mode compensates the time sound takes to go out and come back in. \
                             Measure it: point the microphone at the speakers (or connect the output to the input \
                             with a cable), keep the guitar quiet, and press Measure.",
                            "O modo prática compensa o tempo que o som leva para sair e voltar. \
                             Meça: aponte o microfone para as caixas (ou ligue a saída na entrada \
                             com um cabo), deixe a guitarra em silêncio e clique em Medir.",
                        ),
                    )
                    .small()
                    .color(palette.text_dim),
                );
                ui.horizontal_wrapped(|ui| {
                    if probe.running() {
                        ui.spinner();
                        if let ProbeState::Running { done, of } = probe.state() {
                            ui.label(trf!("Listening for pulse {}/{of}…", "Ouvindo o pulso {}/{of}…", (*done + 1).min(*of)));
                        }
                        if ui.button(tr("Cancel", "Cancelar")).clicked() {
                            out.cancel_latency_probe = true;
                        }
                    } else {
                        let ready = devices.audio_running() && !muted;
                        if ui.add_enabled(ready, egui::Button::new(tr("⏱ Measure latency", "⏱ Medir latência"))).clicked() {
                            out.start_latency_probe = true;
                        }
                        if !ready {
                            ui.label(RichText::new(tr("(needs audio running and unmuted)", "(precisa do áudio ativo e sem mudo)")).color(palette.text_dim));
                        }
                    }
                });
                match probe.state() {
                    ProbeState::Done { latency_secs, heard } => {
                        let measured_ms = (*latency_secs * 1000.0) as f32;
                        let reported_ms = devices.configured_latency_secs * 1000.0;
                        let extra = measured_ms - reported_ms;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(trf!("Round trip: {measured_ms:.1} ms", "Ida e volta: {measured_ms:.1} ms")).strong().color(palette.ok));
                            ui.label(
                                RichText::new(trf!(
                                    "(the driver reports {reported_ms:.1} ms, so {extra:+.1} ms extra; {heard} pulses heard)",
                                    "(o driver informa {reported_ms:.1} ms, então {extra:+.1} ms a mais; {heard} pulsos ouvidos)"
                                ))
                                .color(palette.text_dim),
                            );
                            if ui.button(tr("Use for practice mode", "Usar no modo prática")).clicked() {
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
                    ui.label(trf!(
                        "Practice extra latency: {:+.0} ms",
                        "Latência extra da prática: {:+.0} ms",
                        settings.practice.extra_latency_ms
                    ));
                });
                ui.separator();
                if ui
                    .checkbox(
                        &mut settings.ui.analysis_dry,
                        tr(
                            "Tuner, practice and analyzer listen to the guitar before the pedals",
                            "Afinador, prática e analisador ouvem a guitarra antes dos pedais",
                        ),
                    )
                    .on_hover_text(tr(
                        "Distortion, octavers and delays make pitch detection harder; the dry signal is cleaner",
                        "Distorção, oitavadores e delays dificultam a detecção de notas; o sinal sem pedais é mais limpo",
                    ))
                    .changed()
                {
                    out.analysis_changed = true;
                    save = true;
                }
            });

            section(ui, Section::Levels, &mut |ui| {
                let prefs = &mut settings.ui;
                let gain_label = tr("Input gain", "Ganho de entrada");
                let volume_label = tr("Master volume", "Volume geral");
                let label_width = [gain_label, volume_label]
                    .iter()
                    .map(|text| {
                        egui::WidgetText::from(*text)
                            .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
                            .size()
                            .x
                    })
                    .fold(0.0, f32::max);
                out.levels_changed |= level_row(
                    ui,
                    gain_label,
                    Some(tr(
                        "Boost a weak pickup or tame a hot one before the pedals",
                        "Reforce um captador fraco ou segure um forte antes dos pedais",
                    )),
                    &mut prefs.input_gain_db,
                    -24.0..=24.0,
                    label_width,
                    narrow,
                );
                out.levels_changed |= level_row(ui, volume_label, None, &mut prefs.output_volume_db, -60.0..=12.0, label_width, narrow);
                save |= out.levels_changed;
            });

            section(ui, Section::Appearance, &mut |ui| {
                let prefs = &mut settings.ui;
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("Language:", "Idioma:"));
                    // Each language's name in itself, so it can be found
                    // whichever one is showing.
                    for (language, label) in [
                        (LanguagePref::System, tr("System", "Sistema")),
                        (LanguagePref::English, "English"),
                        (LanguagePref::Portuguese, "Português"),
                    ] {
                        save |= ui.selectable_value(&mut prefs.language, language, label).changed();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("Theme:", "Tema:"));
                    for (theme, label) in [
                        (ThemePref::Dark, tr("Dark", "Escuro")),
                        (ThemePref::Light, tr("Light", "Claro")),
                        (ThemePref::System, tr("System", "Sistema")),
                    ] {
                        save |= ui.selectable_value(&mut prefs.theme, theme, label).changed();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("UI scale:", "Escala da interface:"));
                    for zoom in [0.8, 1.0, 1.25, 1.5, 2.0] {
                        save |= ui
                            .selectable_value(&mut prefs.zoom, zoom, format!("{:.0}%", zoom * 100.0))
                            .changed();
                    }
                    ui.label(RichText::new(tr("(Ctrl + / Ctrl - also zoom)", "(Ctrl + / Ctrl - também mudam o zoom)")).small().color(palette.text_dim));
                });
            });

            section(ui, Section::Midi, &mut |ui| {
                save |= self.midi_ui(ui, settings, chain, midi);
            });

            section(ui, Section::Recordings, &mut |ui| self.recordings_ui(ui));

            section(ui, Section::Shortcuts, &mut |ui| shortcuts_grid(ui));

            section(ui, Section::Files, &mut |ui| {
                let rows: Vec<(&str, Option<std::path::PathBuf>)> = vec![
                    ("Presets", crate::shared::preset::user_preset_dir()),
                    (tr("Settings", "Ajustes"), crate::shared::settings::settings_path()),
                    (tr("App data (downloads, models, tabs)", "Dados do app (downloads, modelos, tabs)"), crate::shared::paths::app_data_dir()),
                ];
                // The path and its Open button (the button first on phones,
                // where the path is truncated to what's left of the row).
                let path_cells = |ui: &mut Ui, path: Option<std::path::PathBuf>, button_first: bool| match path {
                    Some(p) => {
                        let text = |ui: &mut Ui| {
                            ui.add(egui::Label::new(RichText::new(p.display().to_string()).monospace().small()).truncate());
                        };
                        if !button_first {
                            text(ui);
                        }
                        #[cfg(not(target_os = "android"))]
                        {
                            let folder = if p.extension().is_some() { p.parent().map(std::path::Path::to_path_buf) } else { Some(p.clone()) };
                            if let Some(folder) = folder
                                && ui.small_button(tr("Open", "Abrir")).clicked()
                            {
                                open_folder(&folder);
                            }
                        }
                        if button_first {
                            text(ui);
                        }
                    }
                    None => {
                        ui.label(RichText::new(tr("not available on this platform", "indisponível nesta plataforma")).color(palette.text_dim));
                    }
                };
                if narrow {
                    for (label, path) in rows {
                        ui.label(label);
                        ui.horizontal(|ui| path_cells(ui, path, true));
                        ui.add_space(style::space::XS);
                    }
                } else {
                    egui::Grid::new("files_grid").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                        for (label, path) in rows {
                            ui.label(label);
                            path_cells(ui, path, false);
                            ui.end_row();
                        }
                    });
                }
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
            ui.label(tr("Input port:", "Porta de entrada:"));
            // What's left of the row after the refresh button: a combo box
            // doesn't wrap.
            let combo_width = (ui.available_size_before_wrap().x - 60.0).clamp(120.0, 260.0);
            let current = midi.port().unwrap_or(tr("(none)", "(nenhuma)")).to_string();
            let mut picked: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("midi_port").selected_text(current).width(combo_width).show_ui(ui, |ui| {
                if ui.selectable_label(midi.port().is_none(), tr("(none)", "(nenhuma)")).clicked() {
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
            if ui.button("🔄").on_hover_text(tr("Look for MIDI devices again", "Procurar dispositivos MIDI de novo")).clicked() {
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
                ui.colored_label(palette.warn, trf!("MIDI unavailable: {e}", "MIDI indisponível: {e}"));
            }
            Some(Ok(ports)) if ports.is_empty() => {
                ui.label(RichText::new(tr(
                    "No MIDI inputs found. Plug in a controller and press 🔄.",
                    "Nenhuma entrada MIDI encontrada. Conecte um controlador e clique em 🔄.",
                )).color(palette.text_dim));
            }
            _ => {}
        }
        if let Some(error) = midi.error() {
            ui.colored_label(palette.danger, error);
        }
        ui.label(
            RichText::new(tr(
                "Press Learn, then move a knob, press a footswitch or send a program change. \
                 Footswitch CCs toggle on press (value ≥ 64).",
                "Clique em Aprender e então mexa um knob, pise num footswitch ou envie um program change. \
                 CCs de footswitch alternam ao pisar (valor ≥ 64).",
            ))
            .small()
            .color(palette.text_dim),
        );
        let mut actions: Vec<MidiAction> = MidiAction::GLOBAL.to_vec();
        actions.extend((0..chain.len().min(16)).map(MidiAction::TogglePedal));
        let label = |action: MidiAction| match action {
            MidiAction::TogglePedal(i) => trf!("Toggle pedal {} ({})", "Ligar/desligar pedal {} ({})", i + 1, chain[i].description.name()),
            other => other.label(),
        };
        // Three columns don't fit on a phone: the binding goes under its action.
        if ui.ctx().screen_rect().width() < style::NARROW_WIDTH {
            for action in actions {
                ui.label(label(action));
                ui.horizontal(|ui| changed |= self.binding_cells(ui, settings, action));
                ui.add_space(style::space::XS);
            }
            return changed;
        }
        egui::Grid::new("midi_bindings").num_columns(3).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
            for action in actions {
                ui.label(label(action));
                changed |= self.binding_cells(ui, settings, action);
                ui.end_row();
            }
        });
        changed
    }

    /// An action's MIDI trigger and its Learn / Unbind buttons. Returns
    /// whether a binding was removed.
    fn binding_cells(&mut self, ui: &mut Ui, settings: &mut AppSettings, action: MidiAction) -> bool {
        let palette = Palette::of(ui.ctx());
        let mut changed = false;
        let learning = self.learning == Some(action);
        match settings.ui.midi.trigger_for(action) {
            _ if learning => {
                ui.colored_label(palette.accent, tr("waiting for MIDI…", "aguardando MIDI…"));
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
                if ui.small_button(tr("Cancel", "Cancelar")).clicked() {
                    self.learning = None;
                }
            } else if ui.small_button(tr("Learn", "Aprender")).clicked() {
                self.learning = Some(action);
            }
            if settings.ui.midi.trigger_for(action).is_some() && ui.small_button("✖").on_hover_text(tr("Unbind", "Desvincular")).clicked() {
                settings.ui.midi.unbind(action);
                changed = true;
            }
        });
        changed
    }

    fn recordings_ui(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui.ctx());
        let Some(dir) = recordings_dir() else {
            ui.label(RichText::new(tr(
                "Recording needs a data folder, which this platform doesn't have.",
                "A gravação precisa de uma pasta de dados, que esta plataforma não tem.",
            )).color(palette.text_dim));
            return;
        };
        ui.label(
            RichText::new(tr(
                "Press ⏺ in the top bar (or Ctrl+R) to record. Takes are 16-bit mono WAV files.",
                "Clique em ⏺ na barra superior (ou Ctrl+R) para gravar. As gravações são arquivos WAV mono de 16 bits.",
            ))
                .small()
                .color(palette.text_dim),
        );
        ui.horizontal(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("📂 Open folder", "📂 Abrir pasta")).clicked() {
                open_folder(&dir);
            }
            if ui.button("🔄").on_hover_text(tr("Refresh", "Atualizar")).clicked() {
                self.recordings = None;
            }
        });
        let recordings = self.recordings.get_or_insert_with(|| list_recordings(&dir));
        if recordings.is_empty() {
            ui.label(RichText::new(tr("No recordings yet.", "Nenhuma gravação ainda.")).color(palette.text_dim));
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
    // Two columns don't fit on a phone: keys above what they do.
    if ui.available_width() < 480.0 {
        for &(keys, en, pt) in SHORTCUTS {
            ui.label(RichText::new(keys).monospace().color(palette.accent));
            ui.label(if crate::shared::i18n::is_portuguese() { pt } else { en });
            ui.add_space(style::space::XS);
        }
        return;
    }
    egui::Grid::new(ui.id().with("shortcuts")).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for &(keys, en, pt) in SHORTCUTS {
            ui.label(RichText::new(keys).monospace().color(palette.accent));
            ui.label(if crate::shared::i18n::is_portuguese() { pt } else { en });
            ui.end_row();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_its_own_title() {
        let titles: std::collections::HashSet<_> = Section::ALL.iter().map(|s| s.title()).collect();
        assert_eq!(titles.len(), Section::ALL.len());
    }
}
