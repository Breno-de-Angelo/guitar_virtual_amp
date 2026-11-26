//! The app shell: a top bar (preset switcher, level meters, master volume,
//! mute, recorder, metronome), page navigation (a rail on the left on wide
//! windows, a bottom bar on narrow ones such as phones) and the pages.
//!
//! `AudioApp` owns the UI-side copy of the pedal chain; every change is sent
//! whole to the audio thread through `pedal_tx` (see `backend::capture`).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender};
use eframe::egui::{self, Key, Modifiers, RichText, vec2};

use crate::{
    backend::{
        capture::AudioSetup,
        controls::{AudioControls, RecordSource, db_to_gain, gain_to_db},
        midi::MidiInputManager,
        pedals::nam,
        recorder::{Recorder, recordings_dir},
    },
    frontend::{
        devices::DeviceManager,
        lib::latency::LatencyProbe,
        lib::tunings::Tuning,
        style::{self, AppliedStyle, Palette},
        ui::analyzer::AnalyzerPage,
        ui::metronome::MetronomePanel,
        ui::pedalboard::PedalboardPage,
        ui::pedals::looper_cycle,
        ui::presets::{LoadedPreset, PresetPanel, load_preset_ref, preset_order, preset_ref_name, step_preset},
        ui::settings_page::{SettingsPage, shortcuts_grid},
        ui::spotify::{SpotifyAction, SpotifyPanel},
        ui::tab_player::TabPlayerPanel,
        ui::tone3000::{LoadedTone, Tone3000Panel, ToneTarget},
        ui::tuner::{TAB_TUNING, TunerPanel},
        ui::widgets::{MeterState, level_meter},
    },
    shared::{
        midi::{MidiAction, MidiTrigger},
        pedals::{CabinetParams, NamParams, PedalDescription, PedalInstance},
        preset,
        settings::{AppSettings, PresetRef, save_settings},
    },
};

/// Guitar samples kept for the tuner, analyzer and oscilloscope (~85 ms).
const SAMPLE_WINDOW: usize = 4096;

/// Capacity of the `audio_rx` channel the entry points create. The audio
/// thread drops samples when it's full, so it holds about a second: tab
/// practice mode needs a gap-free signal, and a UI frame can be late.
pub const VISUALIZER_CHANNEL_SAMPLES: usize = 48_000;

/// The pages of the app, in navigation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Pedalboard,
    Presets,
    Tuner,
    Tabs,
    Spotify,
    Analyzer,
    Settings,
}

impl Page {
    pub const ALL: [Page; 7] =
        [Page::Pedalboard, Page::Presets, Page::Tuner, Page::Tabs, Page::Spotify, Page::Analyzer, Page::Settings];

    pub fn icon(self) -> &'static str {
        match self {
            Page::Pedalboard => "🎸",
            Page::Presets => "📚",
            Page::Tuner => "🎵",
            Page::Tabs => "🎼",
            Page::Spotify => "🎧",
            Page::Analyzer => "📈",
            Page::Settings => "⚙",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Pedalboard => "Pedals",
            Page::Presets => "Presets",
            Page::Tuner => "Tuner",
            Page::Tabs => "Tabs",
            Page::Spotify => "Spotify",
            Page::Analyzer => "Analyzer",
            Page::Settings => "Settings",
        }
    }
}

/// Parses every NAM model the chain uses into the model cache, on the UI
/// thread, so the audio thread doesn't read files when it rebuilds the chain
/// (see `backend::pedals::nam`). Cache hits are a map lookup, so this is
/// cheap to call on every chain update.
fn preload_nam_models(chain: &[PedalInstance]) {
    let paths: Vec<&str> = chain
        .iter()
        .filter_map(|pedal| match &pedal.description {
            PedalDescription::Nam(params) => params.model_path.as_deref(),
            _ => None,
        })
        .collect();
    for path in &paths {
        let _ = nam::preload(path);
    }
    nam::retain_only(&paths);
}

/// Puts a tone downloaded from TONE3000 into the pedal that asked for it.
/// If that pedal was deleted or replaced meanwhile, appends a new pedal
/// instead so the download isn't lost.
fn apply_loaded_tone(chain: &mut Vec<PedalInstance>, loaded: LoadedTone) {
    let target_pedal = chain.get_mut(loaded.pedal_index).map(|p| &mut p.description);
    match (target_pedal, loaded.target) {
        (Some(PedalDescription::Nam(params)), ToneTarget::Nam) => {
            params.model_path = Some(loaded.path);
            params.source = Some(loaded.source);
        }
        (Some(PedalDescription::Cabinet(params)), ToneTarget::Ir) => {
            params.ir_path = Some(loaded.path);
            params.source = Some(loaded.source);
        }
        (_, ToneTarget::Nam) => chain.push(PedalInstance::new(PedalDescription::Nam(NamParams {
            model_path: Some(loaded.path),
            source: Some(loaded.source),
            ..Default::default()
        }))),
        (_, ToneTarget::Ir) => chain.push(PedalInstance::new(PedalDescription::Cabinet(CabinetParams {
            ir_path: Some(loaded.path),
            source: Some(loaded.source),
            ..Default::default()
        }))),
    }
}

/// The preset the chain was last loaded from, to show its name and whether
/// the chain was edited since.
struct CurrentPreset {
    preset_ref: PresetRef,
    name: String,
    pedals: Vec<PedalInstance>,
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

pub struct AudioApp {
    pub audio_rx: Receiver<i16>,
    pub pedal_tx: Sender<Vec<PedalInstance>>,
    buffer: VecDeque<f64>,
    pedal_chain: Vec<PedalInstance>,
    devices: DeviceManager,
    controls: Arc<AudioControls>,
    recorder: Recorder,
    midi: MidiInputManager,
    preset_panel: PresetPanel,
    metronome_panel: MetronomePanel,
    tone3000_panel: Tone3000Panel,
    tuner: TunerPanel,
    analyzer: AnalyzerPage,
    pedalboard: PedalboardPage,
    settings_page: SettingsPage,
    tab_panel: TabPlayerPanel,
    spotify_panel: SpotifyPanel,
    page: Page,
    current_preset: Option<CurrentPreset>,
    user_muted: bool,
    in_meter: MeterState,
    out_meter: MeterState,
    toast: Option<Toast>,
    show_shortcuts: bool,
    applied_style: Option<AppliedStyle>,
    /// Analysis samples drained since launch (see `lib::latency`).
    received_total: u64,
    latency_probe: LatencyProbe,
    /// Warned about input clipping (once per launch).
    warned_clipping: bool,
    settings: AppSettings,
}

impl AudioApp {
    pub fn new(audio_rx: Receiver<i16>, mut setup: AudioSetup, settings: AppSettings) -> Self {
        let devices = DeviceManager::new(&mut setup, &settings);
        let controls = setup.controls.clone();
        controls.set_input_gain(db_to_gain(settings.ui.input_gain_db));
        controls.set_output_volume(db_to_gain(settings.ui.output_volume_db));
        controls.set_analysis_dry(settings.ui.analysis_dry);

        // Restore the last-loaded preset, if any -- best-effort, silently
        // starting with an empty chain if it's gone missing.
        let current_preset = settings.last_preset.as_ref().and_then(|r| {
            load_preset_ref(r).map(|p| CurrentPreset { preset_ref: r.clone(), name: p.name, pedals: p.pedals })
        });
        let initial_pedal_chain = current_preset.as_ref().map(|c| c.pedals.clone()).unwrap_or_default();
        if !initial_pedal_chain.is_empty() {
            preload_nam_models(&initial_pedal_chain);
            let _ = setup.pedal_tx.send(initial_pedal_chain.clone());
        }

        let mut metronome_panel =
            MetronomePanel::new(setup.metronome_running.clone(), setup.metronome_bpm.clone(), controls.clone());
        metronome_panel.apply_prefs(&settings.ui);

        let mut midi = MidiInputManager::new();
        if let Some(port) = &settings.ui.midi.input_port {
            let _ = midi.connect(port);
        }

        let mut preset_panel = PresetPanel::new();
        if let Some(CurrentPreset { preset_ref: PresetRef::User(_), name, .. }) = &current_preset {
            preset_panel.set_save_name(name);
        }

        Self {
            audio_rx,
            pedal_tx: setup.pedal_tx.clone(),
            buffer: VecDeque::with_capacity(SAMPLE_WINDOW),
            pedal_chain: initial_pedal_chain,
            recorder: Recorder::new(setup.record_rx.clone(), controls.clone()),
            controls,
            midi,
            preset_panel,
            metronome_panel,
            tone3000_panel: Tone3000Panel::new(),
            tuner: TunerPanel::new(),
            analyzer: AnalyzerPage::new(),
            pedalboard: PedalboardPage::default(),
            settings_page: SettingsPage::default(),
            tab_panel: TabPlayerPanel::new(setup.song_player.clone(), &settings),
            spotify_panel: SpotifyPanel::new(),
            page: if settings.last_tab.is_some() { Page::Tabs } else { Page::Pedalboard },
            current_preset,
            user_muted: false,
            in_meter: MeterState::default(),
            out_meter: MeterState::default(),
            toast: None,
            show_shortcuts: false,
            applied_style: None,
            received_total: 0,
            latency_probe: LatencyProbe::new(crate::shared::config::GLOBAL_CONFIG.sample_rate as f64),
            warned_clipping: false,
            devices,
            settings,
        }
    }

    /// Shows `page` (e.g. to open the app on a given page).
    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast { text: text.into(), error, until: Instant::now() + Duration::from_secs(3) });
    }

    /// Replaces the chain with a preset, as an undoable edit.
    fn load_preset(&mut self, loaded: LoadedPreset) {
        let before = std::mem::replace(&mut self.pedal_chain, loaded.pedals.clone());
        self.pedalboard.record_external(&before, &self.pedal_chain);
        if matches!(loaded.preset_ref, PresetRef::User(_)) {
            self.preset_panel.set_save_name(&loaded.name);
        }
        self.toast(format!("Preset: {}", loaded.name), false);
        self.settings.last_preset = Some(loaded.preset_ref.clone());
        save_settings(&self.settings);
        self.current_preset =
            Some(CurrentPreset { preset_ref: loaded.preset_ref, name: loaded.name, pedals: loaded.pedals });
    }

    fn load_preset_ref(&mut self, preset_ref: PresetRef) {
        match load_preset_ref(&preset_ref) {
            Some(p) => self.load_preset(LoadedPreset { pedals: p.pedals, preset_ref, name: p.name }),
            None => self.toast(format!("Preset not found: {}", preset_ref_name(&preset_ref)), true),
        }
    }

    fn step_preset(&mut self, step: isize) {
        let order = preset_order(&self.settings.ui, self.preset_panel.user_presets());
        if let Some(next) = step_preset(&order, self.current_preset.as_ref().map(|c| &c.preset_ref), step) {
            self.load_preset_ref(next);
        }
    }

    fn preset_dirty(&self) -> bool {
        self.current_preset.as_ref().is_some_and(|c| c.pedals != self.pedal_chain)
    }

    /// Ctrl+S: overwrite the loaded user preset, or ask for a name.
    fn save_current_preset(&mut self) {
        if let Some(CurrentPreset { preset_ref: PresetRef::User(path), .. }) = &self.current_preset {
            let path = path.clone();
            match preset::load_from_file(&path) {
                Ok(mut existing) => {
                    existing.pedals = self.pedal_chain.clone();
                    match preset::save_to_file(&existing, &path) {
                        Ok(()) => {
                            if let Some(current) = &mut self.current_preset {
                                current.pedals = self.pedal_chain.clone();
                            }
                            self.preset_panel.refresh();
                            self.toast(format!("Saved \"{}\"", existing.name), false);
                        }
                        Err(e) => self.toast(format!("Save failed: {e}"), true),
                    }
                }
                Err(e) => self.toast(format!("Save failed: {e}"), true),
            }
        } else {
            self.page = Page::Presets;
            self.toast("Name the preset and press Save", false);
        }
    }

    fn set_muted(&mut self, muted: bool) {
        self.user_muted = muted;
        self.toast(if muted { "Output muted (M)" } else { "Output on" }, false);
    }

    fn apply_levels(&self) {
        self.controls.set_input_gain(db_to_gain(self.settings.ui.input_gain_db));
        self.controls.set_output_volume(db_to_gain(self.settings.ui.output_volume_db));
    }

    fn toggle_recording(&mut self) {
        if self.recorder.is_recording() {
            match self.recorder.stop() {
                Some(Ok(path)) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.toast(format!("Recording saved: {name}"), false);
                }
                Some(Err(e)) => self.toast(format!("Recording failed: {e}"), true),
                None => {}
            }
            self.settings_page.invalidate_recordings();
            return;
        }
        if !self.devices.audio_running() {
            self.toast("Audio isn't running: nothing to record", true);
            return;
        }
        let Some(dir) = recordings_dir() else {
            self.toast("No folder to save recordings in", true);
            return;
        };
        let source = if self.settings.ui.record_mix { RecordSource::Mix } else { RecordSource::Guitar };
        match self.recorder.start(&dir, source, self.devices.output_rate()) {
            Ok(_) => self.toast("Recording… (Ctrl+R or ⏹ to stop)", false),
            Err(e) => self.toast(e, true),
        }
    }

    fn toggle_looper(&mut self) -> bool {
        for pedal in &mut self.pedal_chain {
            if let PedalDescription::Looper(params) = &mut pedal.description {
                params.command = looper_cycle(params.command);
                return true;
            }
        }
        self.toast("Add a Looper pedal first", true);
        false
    }

    fn toggle_pedal(&mut self, index: usize) -> bool {
        let Some(pedal) = self.pedal_chain.get_mut(index) else { return false };
        pedal.enabled = !pedal.enabled;
        let text = format!("{} {}", pedal.description.name(), if pedal.enabled { "on" } else { "bypassed" });
        self.toast(text, false);
        true
    }

    /// Runs one controller/shortcut action. Returns whether the chain changed.
    fn perform(&mut self, action: MidiAction, value: u8) -> bool {
        match action {
            MidiAction::TogglePedal(i) => return self.toggle_pedal(i),
            MidiAction::NextPreset => self.step_preset(1),
            MidiAction::PreviousPreset => self.step_preset(-1),
            MidiAction::SetlistSlot => {
                let order = preset_order(&self.settings.ui, self.preset_panel.user_presets());
                if let Some(r) = order.get(value as usize).cloned() {
                    self.load_preset_ref(r);
                }
            }
            MidiAction::ToggleMute => self.set_muted(!self.user_muted),
            MidiAction::TapTempo => self.metronome_panel.tap(&mut self.settings.ui),
            MidiAction::ToggleMetronome => self.metronome_panel.toggle(),
            MidiAction::OutputVolume => {
                // Squared taper: an expression pedal feels even across its travel.
                let gain = (value as f32 / 127.0).powi(2);
                self.settings.ui.output_volume_db = gain_to_db(gain).max(-60.0);
                self.apply_levels();
            }
            MidiAction::LooperCycle => return self.toggle_looper(),
        }
        false
    }

    fn handle_midi(&mut self, trigger: MidiTrigger, value: u8) -> bool {
        if let Some(action) = self.settings_page.learning.take() {
            self.settings.ui.midi.bind(trigger, action);
            save_settings(&self.settings);
            self.toast(format!("{} ➡ {}", trigger.label(), action.label()), false);
            return false;
        }
        let mut changed = false;
        for action in self.settings.ui.midi.actions_for(trigger, value) {
            changed |= self.perform(action, value);
        }
        changed
    }

    /// Global keyboard shortcuts (see `settings_page::SHORTCUTS`). Returns
    /// whether the chain changed.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        if ctx.input_mut(|i| i.consume_key(shift_cmd, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y)) {
            changed |= self.pedalboard.redo(&mut self.pedal_chain);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z)) {
            changed |= self.pedalboard.undo(&mut self.pedal_chain);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.save_current_preset();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) {
            self.toggle_recording();
        }
        let page_keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7];
        for (page, key) in Page::ALL.iter().zip(page_keys) {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key)) {
                self.page = *page;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.show_shortcuts = !self.show_shortcuts;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageDown)) {
            self.step_preset(1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageUp)) {
            self.step_preset(-1);
        }
        // Single letters and digits only when no text field has focus.
        if ctx.wants_keyboard_input() {
            return changed;
        }
        let digit_keys =
            [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (i, key) in digit_keys.into_iter().enumerate() {
            if ctx.input_mut(|inp| inp.consume_key(Modifiers::NONE, key)) {
                changed |= self.toggle_pedal(i);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::M)) {
            self.set_muted(!self.user_muted);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::T)) {
            self.metronome_panel.tap(&mut self.settings.ui);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::K)) {
            self.metronome_panel.toggle();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::L)) {
            changed |= self.toggle_looper();
        }
        changed
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, narrow: bool) -> bool {
        let palette = Palette::of(ui.ctx());
        let mut chain_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // Preset switcher.
            if ui.button("◀").on_hover_text("Previous preset (PageUp)").clicked() {
                self.step_preset(-1);
            }
            let dirty = self.preset_dirty();
            let name = self.current_preset.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "No preset".into());
            let label = RichText::new(format!("{name}{}", if dirty { " •" } else { "" })).strong();
            let label = if self.current_preset.is_some() { label.color(palette.accent) } else { label.color(palette.text_dim) };
            let hover = if dirty { "Edited since loaded (Ctrl+S saves a user preset)" } else { "Open the Presets page" };
            if ui.add(egui::Button::new(label).min_size(vec2(if narrow { 110.0 } else { 160.0 }, 0.0))).on_hover_text(hover).clicked() {
                self.page = Page::Presets;
            }
            if ui.button("▶").on_hover_text("Next preset (PageDown)").clicked() {
                self.step_preset(1);
            }
            if dirty
                && matches!(self.current_preset, Some(CurrentPreset { preset_ref: PresetRef::User(_), .. }))
                && ui.button("💾").on_hover_text("Save changes to this preset (Ctrl+S)").clicked()
            {
                self.save_current_preset();
            }
            ui.separator();

            // Meters.
            let meter = vec2(if narrow { 60.0 } else { 90.0 }, 12.0);
            level_meter(ui, &self.in_meter, meter, "IN");
            level_meter(ui, &self.out_meter, meter, "OUT");

            // Volume and mute.
            // On phones the master volume lives on the Settings page only.
            if !narrow {
                let mut db = self.settings.ui.output_volume_db;
                let slider = ui.add_sized(vec2(100.0, 18.0), egui::Slider::new(&mut db, -60.0..=12.0).show_value(false));
                if slider.changed() {
                    self.settings.ui.output_volume_db = db;
                    self.apply_levels();
                }
                if slider.drag_stopped() {
                    save_settings(&self.settings);
                }
                if slider.double_clicked() {
                    self.settings.ui.output_volume_db = 0.0;
                    self.apply_levels();
                    save_settings(&self.settings);
                }
                slider.on_hover_text(format!("Master volume {db:+.1} dB (double-click: 0 dB)"));
            }
            let muted = self.controls.muted();
            let mute_text = if muted { RichText::new("🔇").color(palette.danger) } else { RichText::new("🔊") };
            if ui.add(egui::Button::new(mute_text).selected(muted)).on_hover_text("Mute (M)").clicked() {
                self.set_muted(!self.user_muted);
            }

            // Recorder.
            let rec_text = match self.recorder.elapsed_secs() {
                Some(secs) => RichText::new(format!("⏹ {}:{:02}", secs as u64 / 60, secs as u64 % 60)).color(palette.danger),
                None => RichText::new("⏺"),
            };
            let source = if self.settings.ui.record_mix { "everything you hear" } else { "the guitar only" };
            let rec = ui
                .add(egui::Button::new(rec_text).selected(self.recorder.is_recording()))
                .on_hover_text(format!("Record {source} to a WAV file (Ctrl+R). Right-click for options."));
            if rec.clicked() {
                self.toggle_recording();
            }
            rec.context_menu(|ui| {
                if ui.radio_value(&mut self.settings.ui.record_mix, false, "Record the guitar only").changed()
                    || ui.radio_value(&mut self.settings.ui.record_mix, true, "Record everything (guitar, click, song)").changed()
                {
                    save_settings(&self.settings);
                }
                #[cfg(not(target_os = "android"))]
                if let Some(dir) = recordings_dir()
                    && ui.button("📂 Open recordings folder").clicked()
                {
                    crate::frontend::ui::settings_page::open_folder(&dir);
                    ui.close();
                }
            });

            // Metronome.
            if self.metronome_panel.compact_indicator(ui).interact(egui::Sense::click()).on_hover_text("Metronome (K starts/stops, T taps)").clicked() {
                self.metronome_panel.toggle();
            }

            if !narrow {
                ui.separator();
                if ui.add_enabled(self.pedalboard.history.can_undo(), egui::Button::new("⟲")).on_hover_text("Undo (Ctrl+Z)").clicked() {
                    chain_changed |= self.pedalboard.undo(&mut self.pedal_chain);
                }
                if ui.add_enabled(self.pedalboard.history.can_redo(), egui::Button::new("⟳")).on_hover_text("Redo (Ctrl+Shift+Z)").clicked() {
                    chain_changed |= self.pedalboard.redo(&mut self.pedal_chain);
                }
                let (buffer_ms, _, cpu) = self.devices.load();
                let color = if cpu > 80.0 { palette.danger } else if cpu > 50.0 { palette.warn } else { palette.text_dim };
                ui.label(RichText::new(format!("{buffer_ms:.1} ms · DSP {cpu:.0}%")).small().color(color))
                    .on_hover_text("Round-trip buffer latency, and how much of each audio callback the pedals use");
                if ui.small_button("?").on_hover_text("Keyboard shortcuts (F1)").clicked() {
                    self.show_shortcuts = true;
                }
            }
        });
        chain_changed
    }

    fn nav_rail(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::of(ui.ctx());
        ui.add_space(style::space::S);
        for page in Page::ALL {
            let selected = self.page == page;
            let text = RichText::new(format!("{}\n{}", page.icon(), page.label()))
                .size(13.0)
                .color(if selected { palette.on_accent } else { palette.text });
            let button = egui::Button::new(text)
                .fill(if selected { palette.accent } else { egui::Color32::TRANSPARENT })
                .min_size(vec2(ui.available_width(), 50.0));
            if ui.add(button).clicked() {
                self.page = page;
            }
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::of(ui.ctx());
        let width = ui.available_width() / Page::ALL.len() as f32;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for page in Page::ALL {
                let selected = self.page == page;
                let text = RichText::new(format!("{}\n{}", page.icon(), page.label()))
                    .size(11.0)
                    .color(if selected { palette.accent } else { palette.text_dim });
                let button = egui::Button::new(text).frame(false).min_size(vec2(width, style::TOUCH_TARGET + 6.0));
                if ui.add(button).clicked() {
                    self.page = page;
                }
            }
        });
    }

    fn page_title(&self, ui: &mut egui::Ui) {
        let title = match self.page {
            Page::Pedalboard => "Pedalboard",
            Page::Presets => "Presets",
            Page::Tuner => "Tuner & metronome",
            Page::Tabs => "Tabs",
            Page::Spotify => "Spotify",
            Page::Analyzer => "Analyzer",
            Page::Settings => "Settings",
        };
        ui.label(RichText::new(title).size(style::text::HEADING + 2.0).strong());
        ui.add_space(style::space::XS);
    }

    /// Draws the current page. Returns `(chain changed, tone choice)`.
    fn central(&mut self, ui: &mut egui::Ui) -> bool {
        let mut chain_changed = false;
        if self.page != Page::Tabs && self.page != Page::Spotify {
            self.page_title(ui);
        }
        match self.page {
            Page::Pedalboard => {
                let out = self.pedalboard.ui(ui, &mut self.pedal_chain);
                chain_changed |= out.chain_changed;
                if let Some((index, request)) = out.tone3000 {
                    self.tone3000_panel.handle_request(index, request);
                }
            }
            Page::Presets => {
                let current = self.current_preset.as_ref().map(|c| c.preset_ref.clone());
                let (loaded, prefs_changed) =
                    self.preset_panel.ui(ui, &self.pedal_chain, current.as_ref(), &mut self.settings.ui);
                if prefs_changed {
                    save_settings(&self.settings);
                }
                if let Some(loaded) = loaded {
                    self.load_preset(loaded);
                    chain_changed = true;
                }
            }
            Page::Tuner => {
                let tab_tuning = self.tab_panel.selected_tuning().and_then(|t| Tuning::from_tab(&t));
                egui::ScrollArea::vertical().id_salt("tuner_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    let palette = Palette::of(ui.ctx());
                    let mut save = false;
                    style::card_frame(&palette).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        save |= self.tuner.ui(ui, &self.buffer, &mut self.settings.ui, tab_tuning);
                    });
                    ui.add_space(style::space::M);
                    style::card_frame(&palette).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        style::section_title(ui, "Metronome");
                        let (pedals, prefs) = self.metronome_panel.ui(ui, &mut self.pedal_chain, &mut self.settings.ui);
                        chain_changed |= pedals;
                        save |= prefs;
                    });
                    if save {
                        save_settings(&self.settings);
                    }
                });
            }
            Page::Tabs => {
                if let Some((pedals, preset_ref)) = self.tab_panel.ui(ui, &mut self.settings) {
                    let name = preset_ref_name(&preset_ref);
                    self.load_preset(LoadedPreset { pedals, preset_ref, name });
                    chain_changed = true;
                }
                if self.tab_panel.take_tuner_request() {
                    self.settings.ui.tuner_tuning = TAB_TUNING.to_string();
                    save_settings(&self.settings);
                    self.page = Page::Tuner;
                }
            }
            Page::Spotify => match self.spotify_panel.ui(ui, &mut self.settings) {
                Some(SpotifyAction::OpenTab(path)) => {
                    self.tab_panel.open_tab(path);
                    self.page = Page::Tabs;
                }
                Some(SpotifyAction::Transcribe { title, artist, track_id, duration_secs }) => {
                    self.tab_panel.start_transcription_for(title, artist, Some(track_id), duration_secs);
                    self.page = Page::Tabs;
                }
                None => {}
            },
            Page::Analyzer => {
                if self.analyzer.ui(ui, &self.buffer, &mut self.settings.ui.analysis_dry) {
                    self.controls.set_analysis_dry(self.settings.ui.analysis_dry);
                    save_settings(&self.settings);
                }
            }
            Page::Settings => {
                let muted = self.controls.muted();
                let out = self.settings_page.ui(
                    ui,
                    &mut self.devices,
                    &self.pedal_chain,
                    &mut self.settings,
                    &mut self.midi,
                    &self.latency_probe,
                    muted,
                );
                if out.levels_changed {
                    self.apply_levels();
                }
                if out.analysis_changed {
                    self.controls.set_analysis_dry(self.settings.ui.analysis_dry);
                }
                if out.start_latency_probe {
                    self.latency_probe.start(self.received_total);
                }
                if out.cancel_latency_probe {
                    self.latency_probe.cancel();
                }
                if let Some(extra) = out.use_extra_latency_ms {
                    self.settings.practice.extra_latency_ms = extra.round();
                    self.tab_panel.set_practice_prefs(&self.settings.practice);
                    save_settings(&self.settings);
                    self.toast(format!("Practice extra latency set to {:+.0} ms", extra), false);
                }
            }
        }
        chain_changed
    }
}

impl eframe::App for AudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Guitar samples since the last frame.
        let mut fresh: Vec<f32> = Vec::with_capacity(self.audio_rx.len());
        while let Ok(sample) = self.audio_rx.try_recv() {
            if self.buffer.len() >= SAMPLE_WINDOW {
                self.buffer.pop_front();
            }
            let normalized = sample as f64 / i16::MAX as f64;
            self.buffer.push_back(normalized);
            fresh.push(normalized as f32);
        }
        // Tab practice mode needs every sample, not just the display window.
        self.tab_panel.feed_input(&fresh, self.devices.configured_latency_secs);
        self.received_total += fresh.len() as u64;
        self.latency_probe.feed(&fresh, &self.controls);
        self.devices.poll();

        let narrow = ctx.screen_rect().width() < style::NARROW_WIDTH;
        style::apply(ctx, &self.settings.ui, narrow, &mut self.applied_style);
        let palette = Palette::of(ctx);

        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        self.in_meter.update(self.controls.take_input_peak(), dt);
        self.out_meter.update(self.controls.take_output_peak(), dt);
        if self.in_meter.clip_secs > 0.0 && !self.warned_clipping {
            self.warned_clipping = true;
            self.toast("The input is clipping: turn the interface gain (or the input gain in Settings) down", true);
        }

        let mut chain_changed = false;
        for (trigger, value) in self.midi.poll() {
            chain_changed |= self.handle_midi(trigger, value);
        }
        chain_changed |= self.handle_shortcuts(ctx);

        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::new().fill(palette.panel).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ctx, |ui| {
                chain_changed |= self.top_bar(ui, narrow);
            });
        if let Some(error) = self.devices.error.clone() {
            egui::TopBottomPanel::top("audio_error")
                .frame(
                    egui::Frame::new()
                        .fill(if palette.dark { egui::Color32::from_rgb(70, 28, 28) } else { egui::Color32::from_rgb(250, 222, 222) })
                        .inner_margin(egui::Margin::symmetric(10, 4)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(palette.danger, format!("⚠ {error}"));
                        if ui.button("Retry").clicked() {
                            self.devices.retry(&self.pedal_chain);
                        }
                        if ui.button("Audio settings").clicked() {
                            self.page = Page::Settings;
                        }
                    });
                });
        }
        if narrow {
            egui::TopBottomPanel::bottom("bottom_nav")
                .frame(egui::Frame::new().fill(palette.panel).inner_margin(egui::Margin::symmetric(0, 2)))
                .show(ctx, |ui| self.bottom_bar(ui));
        } else {
            egui::SidePanel::left("nav_rail")
                .exact_width(92.0)
                .resizable(false)
                .frame(egui::Frame::new().fill(palette.panel).inner_margin(egui::Margin::symmetric(6, 4)))
                .show(ctx, |ui| self.nav_rail(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.background).inner_margin(egui::Margin::same(if narrow { 8 } else { 14 })))
            .show(ctx, |ui| {
                chain_changed |= self.central(ui);
            });

        if let Some(loaded) = self.tone3000_panel.show(ctx) {
            let before = self.pedal_chain.clone();
            apply_loaded_tone(&mut self.pedal_chain, loaded);
            self.pedalboard.record_external(&before, &self.pedal_chain);
            chain_changed = true;
        }

        if self.show_shortcuts {
            let mut open = true;
            egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                shortcuts_grid(ui);
            });
            self.show_shortcuts = open;
        }

        if let Some(toast) = &self.toast {
            if Instant::now() > toast.until {
                self.toast = None;
            } else {
                let bottom = if narrow { -70.0 } else { -16.0 };
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, bottom))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(palette.panel)
                            .stroke(egui::Stroke::new(1.0_f32, if toast.error { palette.danger } else { palette.accent }))
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(&toast.text).color(if toast.error { palette.danger } else { palette.text }));
                            });
                    });
            }
        }

        // Mute while the tuner is open, if asked.
        let tuning_mute = self.page == Page::Tuner && self.settings.ui.mute_while_tuning;
        self.controls.set_muted(self.user_muted || tuning_mute);

        if chain_changed {
            preload_nam_models(&self.pedal_chain);
            let _ = self.pedal_tx.send(self.pedal_chain.clone());
        }

        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.recorder.stop();
        save_settings(&self.settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AudioApp {
        let (audio_tx, audio_rx) = crossbeam::channel::bounded(VISUALIZER_CHANNEL_SAMPLES);
        AudioApp::new(audio_rx, AudioSetup::without_audio(audio_tx), AppSettings::default())
    }

    #[test]
    fn every_page_renders_without_audio() {
        let mut app = app();
        assert!(app.devices.error.is_some(), "no streams → error banner");
        let ctx = egui::Context::default();
        for page in Page::ALL {
            app.page = page;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.central(ui);
                    app.top_bar(ui, false);
                    app.nav_rail(ui);
                    app.bottom_bar(ui);
                });
            });
        }
    }

    #[test]
    fn presets_step_and_dirty_state() {
        let mut app = app();
        app.step_preset(1);
        let first = app.current_preset.as_ref().map(|c| c.name.clone()).expect("a factory preset loaded");
        assert!(!app.preset_dirty());
        app.pedal_chain.push(PedalInstance::new(PedalDescription::Reverb(Default::default())));
        assert!(app.preset_dirty());
        app.step_preset(-1);
        app.step_preset(1);
        assert_eq!(app.current_preset.as_ref().map(|c| c.name.clone()), Some(first));
        assert!(app.pedalboard.undo(&mut app.pedal_chain), "preset loads are undoable");
    }

    #[test]
    fn midi_learn_binds_and_then_dispatches() {
        let mut app = app();
        app.pedal_chain = vec![PedalInstance::new(PedalDescription::Reverb(Default::default()))];
        app.settings_page.learning = Some(MidiAction::TogglePedal(0));
        let switch = MidiTrigger::ControlChange { channel: 0, controller: 80 };
        assert!(!app.handle_midi(switch, 127), "learning doesn't act");
        assert!(app.pedal_chain[0].enabled);
        assert!(app.handle_midi(switch, 127));
        assert!(!app.pedal_chain[0].enabled);
        assert!(!app.handle_midi(switch, 0), "release is ignored");
        let expression = MidiTrigger::ControlChange { channel: 0, controller: 7 };
        app.settings.ui.midi.bind(expression, MidiAction::OutputVolume);
        app.handle_midi(expression, 0);
        assert_eq!(app.settings.ui.output_volume_db, -60.0);
        assert!(app.controls.output_volume() < 0.01);
    }

    #[test]
    fn looper_shortcut_cycles_the_first_looper() {
        use crate::shared::pedals::{LooperCommand, LooperParams};
        let mut app = app();
        assert!(!app.toggle_looper());
        app.pedal_chain = vec![PedalInstance::new(PedalDescription::Looper(LooperParams::default()))];
        assert!(app.toggle_looper());
        assert!(matches!(
            &app.pedal_chain[0].description,
            PedalDescription::Looper(LooperParams { command: LooperCommand::Record, .. })
        ));
    }
}
