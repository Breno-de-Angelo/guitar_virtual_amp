//! Audio device selection, buffer size and stream rebuilds, done on
//! background threads so the UI never blocks on device negotiation.
//! Results come back through one-shot channels polled every frame.

use std::sync::atomic::Ordering;

use cpal::traits::HostTrait;
use crossbeam::channel::{Receiver, Sender, bounded};
use eframe::egui::{self, RichText, Ui};

use crate::backend::capture::{AudioSetup, INPUT_BACKLOG_SAMPLES, OutputShared, switch_input_device, switch_output_device};
use crate::frontend::style::{self, Palette};
use crate::shared::i18n::tr;
use crate::trf;
use crate::shared::config::{BUFFER_SIZE_OPTIONS_FRAMES, GLOBAL_CONFIG};
use crate::shared::pedals::PedalInstance;
use crate::shared::settings::{AppSettings, save_settings};

type StreamResult = Result<cpal::Stream, String>;

fn is_valid_device(name: &str, input: bool) -> bool {
    let lower = name.to_lowercase();
    let blocked = [
        "discard",
        "rate converter",
        "speex",
        "upmix",
        "downmix",
        "plugin",
        "jack",
        "open sound system",
        "oss",
    ];
    // HDMI is output only.
    !(blocked.iter().any(|b| lower.contains(b)) || (input && lower.contains("hdmi")))
}

/// Device names the host offers, filtered and sorted.
pub fn list_devices(input: bool) -> Vec<String> {
    let host = cpal::default_host();
    let devices = if input { host.input_devices() } else { host.output_devices() };
    let mut list: Vec<String> = devices
        .map(|devs| devs.into_iter().map(|d| d.to_string()).filter(|n| is_valid_device(n, input)).collect())
        .unwrap_or_default();
    list.sort();
    list.dedup();
    list
}

/// What the audio streams are doing, for the Settings page and the top bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioStatus {
    /// Both streams run.
    Running,
    /// A device switch, buffer change or restart is in progress.
    Starting,
    /// Neither stream is open.
    Stopped,
    /// The input is open but the output isn't: nothing is processed or played.
    NoOutput,
    /// The output is open but the input isn't: the pedals get silence.
    NoInput,
}

impl AudioStatus {
    pub fn from_streams(input_open: bool, output_open: bool, busy: bool) -> Self {
        match (input_open, output_open) {
            _ if busy => AudioStatus::Starting,
            (true, true) => AudioStatus::Running,
            (false, false) => AudioStatus::Stopped,
            (true, false) => AudioStatus::NoOutput,
            (false, true) => AudioStatus::NoInput,
        }
    }

    pub fn is_running(self) -> bool {
        self == AudioStatus::Running
    }

    /// A few words for the top bar in place of the latency and DSP load;
    /// `None` while audio runs.
    pub fn short_label(self) -> Option<&'static str> {
        match self {
            AudioStatus::Running => None,
            AudioStatus::Starting => Some(tr("starting audio…", "iniciando áudio…")),
            AudioStatus::Stopped => Some(tr("audio off", "áudio desligado")),
            AudioStatus::NoOutput => Some(tr("no output", "sem saída")),
            AudioStatus::NoInput => Some(tr("no input", "sem entrada")),
        }
    }

    /// What's wrong, as a sentence for the Settings page; `None` while audio
    /// runs or is starting.
    pub fn message(self) -> Option<&'static str> {
        match self {
            AudioStatus::Running | AudioStatus::Starting => None,
            AudioStatus::Stopped => Some(tr(
                "Audio stopped: no input or output device is open.",
                "Áudio parado: nenhum dispositivo de entrada ou saída está aberto.",
            )),
            AudioStatus::NoOutput => Some(tr(
                "The output device isn't open: nothing is processed or played.",
                "O dispositivo de saída não está aberto: nada é processado nem tocado.",
            )),
            AudioStatus::NoInput => Some(tr(
                "The input device isn't open: the pedals get no signal.",
                "O dispositivo de entrada não está aberto: os pedais não recebem sinal.",
            )),
        }
    }
}

/// How busy the audio callback is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadLevel {
    Fine,
    High,
    Overloaded,
}

impl LoadLevel {
    /// From the DSP load in % of the callback period.
    pub fn of(cpu_percent: f64) -> Self {
        if cpu_percent > 80.0 {
            LoadLevel::Overloaded
        } else if cpu_percent > 50.0 {
            LoadLevel::High
        } else {
            LoadLevel::Fine
        }
    }

    pub fn color(self, palette: &Palette) -> egui::Color32 {
        match self {
            LoadLevel::Fine => palette.ok,
            LoadLevel::High => palette.warn,
            LoadLevel::Overloaded => palette.danger,
        }
    }
}

/// The live latency and load line of the Settings page.
fn load_text(buffer_ms: f32, queue_ms: f32, cpu: f64) -> String {
    trf!(
        "Round trip {buffer_ms:.1} ms + queue {queue_ms:.1} ms · DSP {cpu:.0}% of the budget",
        "Ida e volta {buffer_ms:.1} ms + fila {queue_ms:.1} ms · DSP {cpu:.0}% do tempo disponível"
    )
}

/// The error to show when `new` couldn't open a stream.
fn startup_error(input_open: bool, output_open: bool) -> Option<&'static str> {
    match (input_open, output_open) {
        (true, true) => None,
        (false, false) => Some(tr(
            "No audio device could be opened. Check the devices below and press Retry.",
            "Nenhum dispositivo de áudio pôde ser aberto. Confira os dispositivos abaixo e clique em Tentar de novo.",
        )),
        (false, true) => Some(tr(
            "The input device couldn't be opened. Check it below and press Retry.",
            "O dispositivo de entrada não pôde ser aberto. Confira-o abaixo e clique em Tentar de novo.",
        )),
        (true, false) => Some(tr(
            "The output device couldn't be opened. Check it below and press Retry.",
            "O dispositivo de saída não pôde ser aberto. Confira-o abaixo e clique em Tentar de novo.",
        )),
    }
}

fn default_device(input: bool) -> Option<String> {
    let host = cpal::default_host();
    let device = if input { host.default_input_device() } else { host.default_output_device() };
    device.map(|d| d.to_string())
}

pub struct DeviceManager {
    pub available_inputs: Vec<String>,
    pub selected_input: String,
    pub available_outputs: Vec<String>,
    pub selected_output: String,
    input_stream: Option<cpal::Stream>,
    output_stream: Option<cpal::Stream>,
    input_tx: Sender<i16>,
    shared: OutputShared,
    pub buffer_frames: u32,
    /// Nominal round-trip buffer latency in seconds.
    pub configured_latency_secs: f32,
    switching_input: Option<Receiver<StreamResult>>,
    switching_output: Option<Receiver<StreamResult>>,
    switching_buffer: Option<Receiver<(StreamResult, StreamResult)>>,
    refreshing_inputs: Option<Receiver<Vec<String>>>,
    refreshing_outputs: Option<Receiver<Vec<String>>>,
    /// Why the last stream (re)build failed, shown on the Settings page and
    /// in a banner.
    pub error: Option<String>,
}

impl DeviceManager {
    pub fn new(setup: &mut AudioSetup, settings: &AppSettings) -> Self {
        let mut available_inputs = list_devices(true);
        let selected_input = settings
            .input_device
            .clone()
            .filter(|name| available_inputs.contains(name))
            .or_else(|| default_device(true))
            .unwrap_or_default();
        if !selected_input.is_empty() && !available_inputs.contains(&selected_input) {
            available_inputs.push(selected_input.clone());
        }
        let mut available_outputs = list_devices(false);
        let selected_output = settings
            .output_device
            .clone()
            .filter(|name| available_outputs.contains(name))
            .or_else(|| default_device(false))
            .unwrap_or_default();
        if !selected_output.is_empty() && !available_outputs.contains(&selected_output) {
            available_outputs.push(selected_output.clone());
        }
        let error = startup_error(setup.input_stream.is_some(), setup.output_stream.is_some()).map(str::to_string);
        Self {
            available_inputs,
            selected_input,
            available_outputs,
            selected_output,
            input_stream: setup.input_stream.take(),
            output_stream: setup.output_stream.take(),
            input_tx: setup.input_tx.clone(),
            shared: setup.output_shared(),
            buffer_frames: settings.buffer_frames.unwrap_or(GLOBAL_CONFIG.buffer_size as u32),
            configured_latency_secs: setup.configured_latency_secs,
            switching_input: None,
            switching_output: None,
            switching_buffer: None,
            refreshing_inputs: None,
            refreshing_outputs: None,
            error,
        }
    }

    /// Sends the chain to the audio thread without ever blocking. The audio
    /// thread applies whole chains, so when the channel is full (no output
    /// stream is draining it: no device, or a switch in progress) the oldest
    /// pending chain is dropped to make room for this one.
    pub fn send_chain(&self, pedal_tx: &Sender<Vec<PedalInstance>>, chain: &[PedalInstance]) {
        let mut message = chain.to_vec();
        for _ in 0..16 {
            match pedal_tx.try_send(message) {
                Ok(()) => return,
                Err(crossbeam::channel::TrySendError::Full(back)) => {
                    let _ = self.shared.pedal_rx.try_recv();
                    message = back;
                }
                Err(crossbeam::channel::TrySendError::Disconnected(_)) => return,
            }
        }
    }

    /// The newest chain waiting for the audio thread (tests).
    #[cfg(test)]
    pub fn take_pending_chain(&self) -> Option<Vec<PedalInstance>> {
        self.shared.pedal_rx.try_iter().last()
    }

    /// Pauses the tab player's song (e.g. so the latency probe hears only its pulse).
    pub fn pause_song(&self) {
        self.shared.song_player.pause();
    }

    /// Sample rate of the output stream (what recordings are written at).
    pub fn output_rate(&self) -> u32 {
        self.shared.song_player.status().output_rate()
    }

    pub fn audio_running(&self) -> bool {
        self.input_stream.is_some() && self.output_stream.is_some()
    }

    /// What the streams are doing. The top bar shows `short_label()` in
    /// place of the latency and DSP load when it isn't `Running`.
    pub fn status(&self) -> AudioStatus {
        AudioStatus::from_streams(self.input_stream.is_some(), self.output_stream.is_some(), self.busy())
    }

    pub fn busy(&self) -> bool {
        self.switching_input.is_some() || self.switching_output.is_some() || self.switching_buffer.is_some()
    }

    /// Applies finished background work. Call every frame.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.switching_input
            && let Ok(result) = rx.try_recv()
        {
            self.switching_input = None;
            match result {
                Ok(stream) => {
                    self.input_stream = Some(stream);
                    self.error = None;
                }
                Err(e) => self.error = Some(trf!("Input device: {e}", "Dispositivo de entrada: {e}")),
            }
        }
        if let Some(rx) = &self.switching_output
            && let Ok(result) = rx.try_recv()
        {
            self.switching_output = None;
            match result {
                Ok(stream) => {
                    self.output_stream = Some(stream);
                    self.error = None;
                }
                Err(e) => self.error = Some(trf!("Output device: {e}", "Dispositivo de saída: {e}")),
            }
        }
        if let Some(rx) = &self.switching_buffer
            && let Ok((input, output)) = rx.try_recv()
        {
            self.switching_buffer = None;
            self.error = None;
            match input {
                Ok(stream) => self.input_stream = Some(stream),
                Err(e) => self.error = Some(trf!("Input device: {e}", "Dispositivo de entrada: {e}")),
            }
            match output {
                Ok(stream) => self.output_stream = Some(stream),
                Err(e) => self.error = Some(trf!("Output device: {e}", "Dispositivo de saída: {e}")),
            }
        }
        if let Some(rx) = &self.refreshing_inputs
            && let Ok(list) = rx.try_recv()
        {
            self.refreshing_inputs = None;
            self.available_inputs = list;
            if !self.selected_input.is_empty() && !self.available_inputs.contains(&self.selected_input) {
                self.available_inputs.push(self.selected_input.clone());
            }
        }
        if let Some(rx) = &self.refreshing_outputs
            && let Ok(list) = rx.try_recv()
        {
            self.refreshing_outputs = None;
            self.available_outputs = list;
            if !self.selected_output.is_empty() && !self.available_outputs.contains(&self.selected_output) {
                self.available_outputs.push(self.selected_output.clone());
            }
        }
    }

    fn spawn_input(&mut self) {
        self.input_stream = None;
        let (tx, rx) = bounded(1);
        let (name, frames, input_tx) = (self.selected_input.clone(), self.buffer_frames, self.input_tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(switch_input_device(&name, frames, input_tx));
        });
        self.switching_input = Some(rx);
    }

    fn spawn_output(&mut self, chain: &[PedalInstance]) {
        self.output_stream = None;
        let (tx, rx) = bounded(1);
        let (name, frames, shared, chain) =
            (self.selected_output.clone(), self.buffer_frames, self.shared.clone(), chain.to_vec());
        std::thread::spawn(move || {
            let _ = tx.send(switch_output_device(&name, frames, shared, chain));
        });
        self.switching_output = Some(rx);
    }

    pub fn switch_input(&mut self, name: String, settings: &mut AppSettings) {
        self.selected_input = name;
        settings.input_device = Some(self.selected_input.clone());
        save_settings(settings);
        self.spawn_input();
    }

    pub fn switch_output(&mut self, name: String, chain: &[PedalInstance], settings: &mut AppSettings) {
        self.selected_output = name;
        settings.output_device = Some(self.selected_output.clone());
        save_settings(settings);
        self.spawn_output(chain);
    }

    /// Rebuilds both streams with a new buffer size.
    pub fn set_buffer_frames(&mut self, frames: u32, chain: &[PedalInstance], settings: &mut AppSettings) {
        self.buffer_frames = frames;
        // The background rebuild may clamp this to what the device supports.
        self.configured_latency_secs = 2.0 * frames as f32 / GLOBAL_CONFIG.sample_rate;
        settings.buffer_frames = Some(frames);
        save_settings(settings);
        self.rebuild_both(chain);
    }

    /// Reopens both streams (after an error, or a device was plugged in).
    pub fn retry(&mut self, chain: &[PedalInstance]) {
        self.available_inputs = list_devices(true);
        self.available_outputs = list_devices(false);
        if !self.available_inputs.contains(&self.selected_input) {
            self.selected_input = default_device(true).unwrap_or_default();
        }
        if !self.available_outputs.contains(&self.selected_output) {
            self.selected_output = default_device(false).unwrap_or_default();
        }
        self.rebuild_both(chain);
    }

    fn rebuild_both(&mut self, chain: &[PedalInstance]) {
        self.input_stream = None;
        self.output_stream = None;
        let (tx, rx) = bounded(1);
        let (input, output, frames) = (self.selected_input.clone(), self.selected_output.clone(), self.buffer_frames);
        let (input_tx, shared, chain) = (self.input_tx.clone(), self.shared.clone(), chain.to_vec());
        std::thread::spawn(move || {
            let input_res = switch_input_device(&input, frames, input_tx);
            let output_res = switch_output_device(&output, frames, shared, chain);
            let _ = tx.send((input_res, output_res));
        });
        self.switching_buffer = Some(rx);
    }

    fn refresh(&mut self, input: bool) {
        let (tx, rx) = bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(list_devices(input));
        });
        if input {
            self.refreshing_inputs = Some(rx);
        } else {
            self.refreshing_outputs = Some(rx);
        }
    }

    /// `(buffer ms, queue ms, CPU % of the callback period)`.
    pub fn load(&self) -> (f32, f32, f64) {
        let buffer_ms = self.configured_latency_secs * 1000.0;
        let callback_ns = self.shared.last_output_callback_ns.load(Ordering::Relaxed);
        // One callback lasts half the round trip.
        let period_ns = (self.configured_latency_secs as f64 / 2.0 * 1e9).max(1.0);
        let cpu = callback_ns as f64 / period_ns * 100.0;
        let queue_ms = INPUT_BACKLOG_SAMPLES.load(Ordering::Relaxed) as f32 * 1000.0 / GLOBAL_CONFIG.sample_rate;
        (buffer_ms, queue_ms, cpu)
    }

    /// The devices section of the Settings page.
    pub fn ui(&mut self, ui: &mut Ui, chain: &[PedalInstance], settings: &mut AppSettings) {
        let palette = Palette::of(ui.ctx());
        let status = self.status();
        // Whatever stops the audio goes first, with the button that fixes it.
        let problem = if self.busy() { None } else { self.error.clone().or(status.message().map(str::to_string)) };
        if let Some(problem) = problem {
            egui::Frame::new()
                .fill(palette.danger.gamma_multiply(0.15))
                .stroke(egui::Stroke::new(1.0_f32, palette.danger.gamma_multiply(0.6)))
                .corner_radius(egui::CornerRadius::same(5))
                .inner_margin(egui::Margin::same(style::space::S as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let text = RichText::new(format!("⚠ {problem}")).strong().color(palette.danger);
                    let mut retry = |ui: &mut Ui| {
                        if ui.button(tr("Retry", "Tentar de novo")).on_hover_text(tr("Reopen both devices", "Reabrir os dois dispositivos")).clicked() {
                            self.retry(chain);
                        }
                    };
                    // A wrapped label and a button in one row overlap, so
                    // the button goes under the text on narrow windows.
                    if ui.ctx().screen_rect().width() < style::NARROW_WIDTH {
                        ui.label(text);
                        retry(ui);
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(text);
                            retry(ui);
                        });
                    }
                });
            ui.add_space(style::space::XS);
        }
        // Room for the label column, the refresh button and the spacing,
        // so the row fits on a phone.
        let combo_width = (ui.available_width() - 140.0).clamp(120.0, 280.0);
        egui::Grid::new("device_grid").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tr("Input", "Entrada"));
            if self.switching_input.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Connecting...", "Conectando..."));
                });
            } else {
                let mut picked = None;
                egui::ComboBox::from_id_salt("input_device")
                    .selected_text(if self.selected_input.is_empty() { tr("(none)", "(nenhum)") } else { &self.selected_input })
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        for name in &self.available_inputs {
                            if ui.selectable_label(*name == self.selected_input, name).clicked() {
                                picked = Some(name.clone());
                            }
                        }
                    });
                if let Some(name) = picked {
                    self.switch_input(name, settings);
                }
            }
            if self.refreshing_inputs.is_some() {
                ui.spinner();
            } else if ui.button("🔄").on_hover_text(tr("Refresh the device list", "Atualizar a lista de dispositivos")).clicked() {
                self.refresh(true);
            }
            ui.end_row();

            ui.label(tr("Output", "Saída"));
            if self.switching_output.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Connecting...", "Conectando..."));
                });
            } else {
                let mut picked = None;
                egui::ComboBox::from_id_salt("output_device")
                    .selected_text(if self.selected_output.is_empty() { tr("(none)", "(nenhum)") } else { &self.selected_output })
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        for name in &self.available_outputs {
                            if ui.selectable_label(*name == self.selected_output, name).clicked() {
                                picked = Some(name.clone());
                            }
                        }
                    });
                if let Some(name) = picked {
                    self.switch_output(name, chain, settings);
                }
            }
            if self.refreshing_outputs.is_some() {
                ui.spinner();
            } else if ui.button("🔄").on_hover_text(tr("Refresh the device list", "Atualizar a lista de dispositivos")).clicked() {
                self.refresh(false);
            }
            ui.end_row();

            ui.label("Buffer");
            if self.switching_buffer.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Applying...", "Aplicando..."));
                });
            } else {
                let mut selected = self.buffer_frames;
                egui::ComboBox::from_id_salt("buffer_size")
                    .selected_text(format!(
                        "{selected} frames ({:.1} ms)",
                        selected as f32 * 1000.0 / GLOBAL_CONFIG.sample_rate
                    ))
                    .show_ui(ui, |ui| {
                        for &frames in BUFFER_SIZE_OPTIONS_FRAMES {
                            ui.selectable_value(
                                &mut selected,
                                frames,
                                format!("{frames} frames ({:.1} ms)", frames as f32 * 1000.0 / GLOBAL_CONFIG.sample_rate),
                            );
                        }
                    })
                    .response
                    .on_hover_text(tr(
                        "Smaller = less latency but more risk of crackles. The device may clamp it.",
                        "Menor = menos latência, mas mais risco de estalos. O dispositivo pode limitar o valor.",
                    ));
                if selected != self.buffer_frames {
                    self.set_buffer_frames(selected, chain, settings);
                }
            }
            ui.label("");
            ui.end_row();
        });
        // The live numbers only while both streams run; the problem box
        // above covers the rest, and the grid's spinners a switch.
        if status.is_running() {
            ui.horizontal_wrapped(|ui| {
                let (buffer_ms, queue_ms, cpu) = self.load();
                ui.colored_label(LoadLevel::of(cpu).color(&palette), load_text(buffer_ms, queue_ms, cpu));
                if ui.button(tr("Restart audio", "Reiniciar áudio")).on_hover_text(tr("Reopen both devices", "Reabrir os dois dispositivos")).clicked() {
                    self.retry(chain);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_pseudo_devices() {
        assert!(is_valid_device("USB Audio CODEC", true));
        assert!(!is_valid_device("Discard all samples", true));
        assert!(!is_valid_device("HDMI 0", true));
        assert!(is_valid_device("HDMI 0", false));
        assert!(!is_valid_device("JACK Audio Connection Kit", false));
    }

    #[test]
    fn status_follows_the_streams() {
        assert_eq!(AudioStatus::from_streams(true, true, false), AudioStatus::Running);
        assert_eq!(AudioStatus::from_streams(false, false, false), AudioStatus::Stopped);
        assert_eq!(AudioStatus::from_streams(true, false, false), AudioStatus::NoOutput);
        assert_eq!(AudioStatus::from_streams(false, true, false), AudioStatus::NoInput);
        assert_eq!(AudioStatus::from_streams(false, false, true), AudioStatus::Starting);
        assert_eq!(AudioStatus::from_streams(true, true, true), AudioStatus::Starting);
    }

    #[test]
    fn only_a_running_status_has_no_label() {
        assert!(AudioStatus::Running.is_running());
        assert_eq!(AudioStatus::Running.short_label(), None);
        assert_eq!(AudioStatus::Running.message(), None);
        assert_eq!(AudioStatus::Stopped.short_label(), Some("audio off"));
        assert_eq!(AudioStatus::Starting.message(), None);
        for status in [AudioStatus::Stopped, AudioStatus::NoOutput, AudioStatus::NoInput] {
            assert!(!status.is_running());
            assert!(status.short_label().is_some());
            assert!(status.message().is_some());
        }
    }

    #[test]
    fn load_levels_and_text() {
        assert_eq!(LoadLevel::of(0.0), LoadLevel::Fine);
        assert_eq!(LoadLevel::of(50.0), LoadLevel::Fine);
        assert_eq!(LoadLevel::of(60.0), LoadLevel::High);
        assert_eq!(LoadLevel::of(95.0), LoadLevel::Overloaded);
        let palette = Palette::dark();
        assert_eq!(LoadLevel::Overloaded.color(&palette), palette.danger);
        assert_eq!(load_text(42.66, 0.0, 3.4), "Round trip 42.7 ms + queue 0.0 ms · DSP 3% of the budget");
    }

    #[test]
    fn startup_error_names_the_missing_side() {
        assert_eq!(startup_error(true, true), None);
        assert!(startup_error(false, false).unwrap().starts_with("No audio device"));
        assert!(startup_error(false, true).unwrap().contains("input"));
        assert!(startup_error(true, false).unwrap().contains("output"));
    }
}
