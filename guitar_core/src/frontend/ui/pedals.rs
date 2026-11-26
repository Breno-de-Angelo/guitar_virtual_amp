//! One pedal of the chain, drawn as a stompbox card: a colored stripe for
//! its effect family, a bypass LED, knobs for its parameters and a
//! footswitch. `pedal_card` returns what the user did as a [`PedalAction`];
//! the pedalboard (`frontend::ui::pedalboard`) applies it to the chain.

use eframe::egui::{self, Color32, Response, RichText, Sense, Ui, vec2};

use crate::backend::pedals::nam;
use crate::frontend::style::{self, Palette};
use crate::frontend::ui::tone3000::{
    Tone3000Request, ToneTarget, browse_button, nam_warning, source_block,
};
use crate::frontend::ui::widgets::{Knob, footswitch, knob_usize, led};
use crate::shared::pedals::{
    AmpParams, AmpVoicing, CabinetParams, ChorusParams, CompressorParams, DelayParams,
    DistortionParams, EqParams, FlangerParams, LooperCommand, LooperParams, LowPassParams,
    NamParams, NoiseGateParams, OctaverParams, PedalDescription, PedalInstance, PhaserParams,
    PitchShifterParams, ReverbParams, TremoloParams, Tone3000Source, WahWahParams,
};

#[derive(Debug, PartialEq)]
pub enum PedalAction {
    None,
    Updated,
    Deleted,
    /// Insert a copy right after this pedal.
    Duplicate,
    /// Move this many places (negative = earlier in the chain).
    Move(isize),
    /// The pedal wants the TONE3000 window (browse, or re-download a tone).
    Tone3000(Tone3000Request),
}

/// What [`pedal_card`] drew, besides the action.
pub struct PedalCard {
    pub action: PedalAction,
    /// The drag handle (for reordering by drag and drop).
    pub handle: Response,
    /// The whole card, for drop targets.
    pub rect: egui::Rect,
}

/// Draws one pedal card. `index`/`count` place it in the chain (for the
/// move buttons and the collapse state).
/// `width` is the card's outer width (`style::PEDAL_CARD_WIDTH` on the
/// grid, the screen width on phones).
pub fn pedal_card(ui: &mut Ui, index: usize, count: usize, instance: &mut PedalInstance, width: f32) -> PedalCard {
    let palette = Palette::of(ui.ctx());
    let stripe = palette.category(instance.description.category());
    let collapsed_id = ui.id().with(("pedal_collapsed", index));
    let mut collapsed = ui.data(|d| d.get_temp::<bool>(collapsed_id)).unwrap_or(false);
    let mut action = PedalAction::None;
    let mut handle = None;

    let frame = style::card_frame(&palette)
        .inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 8 })
        .fill(if instance.enabled { palette.card } else { palette.card.gamma_multiply(0.75) });
    let inner = frame.show(ui, |ui| {
        ui.set_width(width - 22.0);
        ui.set_max_width(width - 22.0);
        ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let h = ui
                .add(egui::Label::new(RichText::new("☰").color(palette.text_dim)).sense(Sense::drag()))
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("Drag to reorder");
            handle = Some(h);
            if led(ui, instance.enabled, 13.0, palette.led_on)
                .on_hover_text(if instance.enabled { "On (click to bypass)" } else { "Bypassed (click to turn on)" })
                .clicked()
            {
                instance.enabled = !instance.enabled;
                action = PedalAction::Updated;
            }
            let title = ui
                .add(egui::Label::new(RichText::new(instance.description.name()).strong().color(palette.text)).sense(Sense::click()))
                .on_hover_text(format!("{}\nClick to {} the knobs", instance.description.blurb(), if collapsed { "show" } else { "hide" }));
            if title.clicked() {
                collapsed = !collapsed;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("…", |ui| {
                    if ui.add_enabled(index > 0, egui::Button::new("◀ Move earlier")).clicked() {
                        action = PedalAction::Move(-1);
                    }
                    if ui.add_enabled(index + 1 < count, egui::Button::new("▶ Move later")).clicked() {
                        action = PedalAction::Move(1);
                    }
                    if ui.button("📋 Duplicate").clicked() {
                        action = PedalAction::Duplicate;
                    }
                    if ui.button("↺ Reset to defaults").clicked() {
                        reset_to_defaults(&mut instance.description);
                        action = PedalAction::Updated;
                    }
                    if ui.button(if collapsed { "⏷ Expand" } else { "⏶ Collapse" }).clicked() {
                        collapsed = !collapsed;
                    }
                    ui.separator();
                    if ui.button(RichText::new("🗑 Remove").color(palette.danger)).clicked() {
                        action = PedalAction::Deleted;
                    }
                })
                .response
                .on_hover_text("Pedal options");
            });
        });

        if !collapsed {
            ui.add_space(style::space::XS);
            let (changed, request) = pedal_controls(ui, &mut instance.description, stripe);
            if let Some(request) = request {
                action = PedalAction::Tone3000(request);
            } else if changed && action == PedalAction::None {
                action = PedalAction::Updated;
            }
            ui.add_space(style::space::XS);
            ui.vertical_centered(|ui| {
                if footswitch(ui, instance.enabled)
                    .on_hover_text(if instance.enabled { "Stomp to bypass" } else { "Stomp to turn on" })
                    .clicked()
                {
                    instance.enabled = !instance.enabled;
                    action = PedalAction::Updated;
                }
            });
        }
        });
    });
    ui.data_mut(|d| d.insert_temp(collapsed_id, collapsed));

    // Family stripe along the top edge.
    let rect = inner.response.rect;
    let stripe_rect = egui::Rect::from_min_size(rect.min + vec2(1.0, 1.0), vec2(rect.width() - 2.0, 5.0));
    ui.painter().rect_filled(
        stripe_rect,
        egui::CornerRadius { nw: style::CARD_RADIUS, ne: style::CARD_RADIUS, sw: 0, se: 0 },
        if instance.enabled { stripe } else { stripe.gamma_multiply(0.35) },
    );

    PedalCard { action, handle: handle.expect("drawn above"), rect }
}

/// One pedal card on its own (outside a chain), returning only the action.
#[cfg(test)]
pub fn render_pedal_ui(ui: &mut Ui, instance: &mut PedalInstance) -> PedalAction {
    pedal_card(ui, 0, 1, instance, style::PEDAL_CARD_WIDTH).action
}

/// Puts the pedal's parameters back to their defaults, keeping loaded files
/// (IR, NAM model) since those are the pedal's identity.
fn reset_to_defaults(description: &mut PedalDescription) {
    match description {
        PedalDescription::Amp(p) => *p = AmpParams::default(),
        PedalDescription::Delay(p) => *p = DelayParams::default(),
        PedalDescription::Reverb(p) => *p = ReverbParams::default(),
        PedalDescription::LowPass(p) => *p = LowPassParams::default(),
        PedalDescription::Flanger(p) => *p = FlangerParams::default(),
        PedalDescription::WahWah(p) => *p = WahWahParams::default(),
        PedalDescription::Distortion(p) => *p = DistortionParams::default(),
        PedalDescription::NoiseGate(p) => *p = NoiseGateParams::default(),
        PedalDescription::Compressor(p) => *p = CompressorParams::default(),
        PedalDescription::Tremolo(p) => *p = TremoloParams::default(),
        PedalDescription::Chorus(p) => *p = ChorusParams::default(),
        PedalDescription::Eq(p) => *p = EqParams::default(),
        PedalDescription::Phaser(p) => *p = PhaserParams::default(),
        PedalDescription::Octaver(p) => *p = OctaverParams::default(),
        PedalDescription::PitchShift(p) => *p = PitchShifterParams::default(),
        PedalDescription::Looper(p) => {
            p.max_loop_seconds = LooperParams::default().max_loop_seconds;
        }
        PedalDescription::Cabinet(p) => p.mix = CabinetParams::default().mix,
        PedalDescription::Nam(p) => {
            let d = NamParams::default();
            p.input_gain_db = d.input_gain_db;
            p.output_gain_db = d.output_gain_db;
            p.normalize = d.normalize;
        }
    }
}

/// The knobs and buttons of one pedal. Returns whether a parameter changed,
/// and a TONE3000 request if the pedal asked for one.
pub fn pedal_controls(
    ui: &mut Ui,
    pedal: &mut PedalDescription,
    color: Color32,
) -> (bool, Option<Tone3000Request>) {
    let mut request = None;
    let changed = match pedal {
        PedalDescription::Amp(params) => amp_ui(ui, params, color),
        PedalDescription::Delay(params) => delay_ui(ui, params, color),
        PedalDescription::Reverb(params) => reverb_ui(ui, params, color),
        PedalDescription::LowPass(params) => low_pass_ui(ui, params, color),
        PedalDescription::Flanger(params) => flanger_ui(ui, params, color),
        PedalDescription::WahWah(params) => wah_wah_ui(ui, params, color),
        PedalDescription::Distortion(params) => distortion_ui(ui, params, color),
        PedalDescription::NoiseGate(params) => noise_gate_ui(ui, params, color),
        PedalDescription::Compressor(params) => compressor_ui(ui, params, color),
        PedalDescription::Tremolo(params) => tremolo_ui(ui, params, color),
        PedalDescription::Chorus(params) => chorus_ui(ui, params, color),
        PedalDescription::Eq(params) => eq_ui(ui, params, color),
        PedalDescription::Phaser(params) => phaser_ui(ui, params, color),
        PedalDescription::Octaver(params) => octaver_ui(ui, params, color),
        PedalDescription::PitchShift(params) => pitch_shifter_ui(ui, params, color),
        PedalDescription::Looper(params) => looper_ui(ui, params, color),
        PedalDescription::Cabinet(params) => {
            let (updated, cabinet_request) = cabinet_ui(ui, params, color);
            request = cabinet_request;
            updated
        }
        PedalDescription::Nam(params) => {
            let (updated, nam_request) = nam_ui(ui, params, color);
            request = nam_request;
            updated
        }
    };
    (changed, request)
}

/// Lays knobs out in rows of three.
fn knobs(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        add(ui)
    })
    .inner
}

/// Shorthand for the common knob shapes.
struct K {
    color: Color32,
}

impl K {
    fn lin(&self, ui: &mut Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str, default: f32) -> bool {
        ui.add(Knob::new(v, range, label).default_value(default).color(self.color)).changed()
    }
    fn unit(&self, ui: &mut Ui, v: &mut f32, label: &str, default: f32) -> bool {
        self.lin(ui, v, 0.0..=1.0, label, default)
    }
    fn db(&self, ui: &mut Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str, default: f32) -> bool {
        ui.add(Knob::new(v, range, label).default_value(default).suffix(" dB").decimals(1).color(self.color))
            .changed()
    }
    fn log(
        &self,
        ui: &mut Ui,
        v: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        label: &str,
        suffix: &str,
        default: f32,
    ) -> bool {
        let decimals = if *range.end() > 100.0 { 0 } else { 2 };
        ui.add(
            Knob::new(v, range, label)
                .logarithmic(true)
                .suffix(suffix)
                .decimals(decimals)
                .default_value(default)
                .color(self.color),
        )
        .changed()
    }
}

fn amp_ui(ui: &mut Ui, params: &mut AmpParams, color: Color32) -> bool {
    let d = AmpParams::default();
    let k = K { color };
    let mut changed = false;
    ui.horizontal(|ui| {
        for (voicing, label) in [(AmpVoicing::Clean, "Clean"), (AmpVoicing::Crunch, "Crunch"), (AmpVoicing::Lead, "Lead")] {
            changed |= ui.selectable_value(&mut params.voicing, voicing, label).changed();
        }
    });
    changed |= knobs(ui, |ui| {
        let mut c = k.lin(ui, &mut params.gain, 0.0..=5.0, "Gain", d.gain);
        c |= k.db(ui, &mut params.bass, -12.0..=12.0, "Bass", d.bass);
        c |= k.db(ui, &mut params.mid, -12.0..=12.0, "Mid", d.mid);
        c |= k.db(ui, &mut params.treble, -12.0..=12.0, "Treble", d.treble);
        c
    });
    changed
}

pub fn delay_ui(ui: &mut Ui, params: &mut DelayParams, color: Color32) -> bool {
    let d = DelayParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.log(ui, &mut params.delay_ms, 1.0..=2000.0, "Time", " ms", d.delay_ms);
        c |= k.lin(ui, &mut params.feedback, 0.0..=0.99, "Feedback", d.feedback);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c |= k.unit(ui, &mut params.gain, "Level", d.gain);
        c |= k.lin(ui, &mut params.damping, 0.0..=0.99, "Damping", d.damping);
        c |= knob_usize(ui, &mut params.taps, 1..=8, "Taps", d.taps).changed();
        c
    })
}

pub fn reverb_ui(ui: &mut Ui, params: &mut ReverbParams, color: Color32) -> bool {
    let d = ReverbParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.lin(ui, &mut params.room_size, 0.0..=0.99, "Room", d.room_size);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c |= k.lin(ui, &mut params.damping, 0.0..=0.99, "Damping", d.damping);
        c
    })
}

fn low_pass_ui(ui: &mut Ui, params: &mut LowPassParams, color: Color32) -> bool {
    let d = LowPassParams::default();
    let k = K { color };
    knobs(ui, |ui| k.log(ui, &mut params.frequency, 50.0..=10_000.0, "Cutoff", " Hz", d.frequency))
}

fn flanger_ui(ui: &mut Ui, params: &mut FlangerParams, color: Color32) -> bool {
    let d = FlangerParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = ui
            .add(Knob::new(&mut params.delay_range, 0.0..=10.0, "Range").suffix(" ms").decimals(1).default_value(d.delay_range).color(color))
            .changed();
        c |= ui
            .add(Knob::new(&mut params.delay_rate, 0.0..=5.0, "Rate").suffix(" Hz").default_value(d.delay_rate).color(color))
            .changed();
        c |= k.unit(ui, &mut params.gain, "Depth", d.gain);
        c
    })
}

fn wah_wah_ui(ui: &mut Ui, params: &mut WahWahParams, color: Color32) -> bool {
    let d = WahWahParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.log(ui, &mut params.frequency, 100.0..=8000.0, "Freq", " Hz", d.frequency);
        c |= k.log(ui, &mut params.resonance, 0.1..=10.0, "Q", "", d.resonance);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c |= k.log(ui, &mut params.lfo_rate, 0.1..=10.0, "Rate", " Hz", d.lfo_rate);
        c |= k.unit(ui, &mut params.lfo_depth, "Depth", d.lfo_depth);
        c
    })
}

fn pitch_shifter_ui(ui: &mut Ui, params: &mut PitchShifterParams, color: Color32) -> bool {
    let d = PitchShifterParams::default();
    let k = K { color };
    let mut changed = knobs(ui, |ui| {
        let mut c = ui
            .add(
                Knob::new(&mut params.semitones, -12.0..=12.0, "Shift")
                    .suffix(" st")
                    .decimals(1)
                    .default_value(d.semitones)
                    .color(color),
            )
            .changed();
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c |= k.log(ui, &mut params.grain_size_ms, 20.0..=200.0, "Grain", " ms", d.grain_size_ms);
        c
    });
    ui.horizontal(|ui| {
        for (label, st) in [("-12", -12.0), ("-5", -5.0), ("+5", 5.0), ("+7", 7.0), ("+12", 12.0)] {
            if ui.small_button(label).on_hover_text("Set the interval in semitones").clicked() {
                params.semitones = st;
                changed = true;
            }
        }
    });
    changed
}

fn distortion_ui(ui: &mut Ui, params: &mut DistortionParams, color: Color32) -> bool {
    let d = DistortionParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.lin(ui, &mut params.drive, 0.0..=10.0, "Drive", d.drive);
        c |= k.unit(ui, &mut params.tone, "Tone", d.tone);
        c |= k.lin(ui, &mut params.level, 0.0..=2.0, "Level", d.level);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c |= k.lin(ui, &mut params.bias, -1.0..=1.0, "Bias", d.bias);
        c
    })
}

fn noise_gate_ui(ui: &mut Ui, params: &mut NoiseGateParams, color: Color32) -> bool {
    let d = NoiseGateParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = ui
            .add(
                Knob::new(&mut params.threshold, 0.001..=0.5, "Threshold")
                    .logarithmic(true)
                    .decimals(3)
                    .default_value(d.threshold)
                    .color(color),
            )
            .changed();
        c |= k.log(ui, &mut params.attack_ms, 0.1..=50.0, "Attack", " ms", d.attack_ms);
        c |= k.log(ui, &mut params.release_ms, 1.0..=1000.0, "Release", " ms", d.release_ms);
        c |= ui
            .add(Knob::new(&mut params.hold_ms, 0.0..=500.0, "Hold").suffix(" ms").decimals(0).default_value(d.hold_ms).color(color))
            .changed();
        c
    })
}

fn compressor_ui(ui: &mut Ui, params: &mut CompressorParams, color: Color32) -> bool {
    let d = CompressorParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.unit(ui, &mut params.threshold, "Threshold", d.threshold);
        c |= ui
            .add(Knob::new(&mut params.ratio, 1.0..=20.0, "Ratio").logarithmic(true).suffix(":1").decimals(1).default_value(d.ratio).color(color))
            .changed();
        c |= k.log(ui, &mut params.attack_ms, 0.1..=100.0, "Attack", " ms", d.attack_ms);
        c |= k.log(ui, &mut params.release_ms, 1.0..=1000.0, "Release", " ms", d.release_ms);
        c |= k.lin(ui, &mut params.makeup_gain, 0.0..=5.0, "Makeup", d.makeup_gain);
        c
    })
}

fn tremolo_ui(ui: &mut Ui, params: &mut TremoloParams, color: Color32) -> bool {
    let d = TremoloParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.log(ui, &mut params.rate_hz, 0.5..=10.0, "Rate", " Hz", d.rate_hz);
        c |= k.unit(ui, &mut params.depth, "Depth", d.depth);
        c
    })
}

fn chorus_ui(ui: &mut Ui, params: &mut ChorusParams, color: Color32) -> bool {
    let d = ChorusParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = ui
            .add(Knob::new(&mut params.delay_ms, 5.0..=40.0, "Delay").suffix(" ms").decimals(1).default_value(d.delay_ms).color(color))
            .changed();
        c |= ui
            .add(Knob::new(&mut params.depth_ms, 0.0..=15.0, "Depth").suffix(" ms").decimals(1).default_value(d.depth_ms).color(color))
            .changed();
        c |= k.log(ui, &mut params.rate_hz, 0.1..=5.0, "Rate", " Hz", d.rate_hz);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c
    })
}

fn eq_ui(ui: &mut Ui, params: &mut EqParams, color: Color32) -> bool {
    let d = EqParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.db(ui, &mut params.low_gain_db, -12.0..=12.0, "Low", d.low_gain_db);
        c |= k.db(ui, &mut params.mid_gain_db, -12.0..=12.0, "Mid", d.mid_gain_db);
        c |= k.db(ui, &mut params.high_gain_db, -12.0..=12.0, "High", d.high_gain_db);
        c
    })
}

fn phaser_ui(ui: &mut Ui, params: &mut PhaserParams, color: Color32) -> bool {
    let d = PhaserParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.log(ui, &mut params.rate_hz, 0.05..=5.0, "Rate", " Hz", d.rate_hz);
        c |= k.unit(ui, &mut params.depth, "Depth", d.depth);
        c |= k.lin(ui, &mut params.feedback, 0.0..=0.95, "Feedback", d.feedback);
        c |= k.unit(ui, &mut params.mix, "Mix", d.mix);
        c
    })
}

fn octaver_ui(ui: &mut Ui, params: &mut OctaverParams, color: Color32) -> bool {
    let d = OctaverParams::default();
    let k = K { color };
    knobs(ui, |ui| {
        let mut c = k.unit(ui, &mut params.octave_down_mix, "Octave", d.octave_down_mix);
        c |= k.unit(ui, &mut params.dry_mix, "Dry", d.dry_mix);
        c |= k.unit(ui, &mut params.tone, "Tone", d.tone);
        c
    })
}

fn looper_ui(ui: &mut Ui, params: &mut LooperParams, color: Color32) -> bool {
    let palette = Palette::of(ui.ctx());
    let mut changed = false;
    let (status, status_color) = match params.command {
        LooperCommand::Idle | LooperCommand::Clear => ("Empty", palette.text_dim),
        LooperCommand::Record => ("⏺ Recording", palette.danger),
        LooperCommand::Play => ("▶ Playing", palette.ok),
        LooperCommand::Overdub => ("⏺ Overdubbing", palette.warn),
        LooperCommand::Stop => ("■ Stopped", palette.text_dim),
    };
    ui.label(RichText::new(status).color(status_color).strong());
    ui.horizontal_wrapped(|ui| {
        let buttons = [
            ("⏺", LooperCommand::Record, "Record"),
            ("▶", LooperCommand::Play, "Play"),
            ("⏺+", LooperCommand::Overdub, "Overdub"),
            ("⏹", LooperCommand::Stop, "Stop"),
            ("🗑", LooperCommand::Clear, "Clear the loop"),
        ];
        for (label, command, hover) in buttons {
            let selected = params.command == command;
            if ui.selectable_label(selected, label).on_hover_text(hover).clicked() {
                params.command = command;
                changed = true;
            }
        }
    });
    let d = LooperParams::default();
    changed |= knobs(ui, |ui| {
        ui.add(
            Knob::new(&mut params.max_loop_seconds, 1.0..=60.0, "Max length")
                .suffix(" s")
                .decimals(0)
                .default_value(d.max_loop_seconds)
                .color(color),
        )
        .changed()
    });
    changed
}

/// The next looper command in the record → play → overdub → play cycle
/// of a one-button looper (MIDI footswitch, keyboard shortcut).
pub fn looper_cycle(command: LooperCommand) -> LooperCommand {
    match command {
        LooperCommand::Idle | LooperCommand::Clear | LooperCommand::Stop => LooperCommand::Record,
        LooperCommand::Record | LooperCommand::Overdub => LooperCommand::Play,
        LooperCommand::Play => LooperCommand::Overdub,
    }
}

/// File name of `path` for display, falling back to the whole path.
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Shows the loaded file, or a "download again" button when the file is
/// gone but the pedal remembers which TONE3000 tone it came from.
fn loaded_file_ui(
    ui: &mut Ui,
    path: Option<&str>,
    source: Option<&Tone3000Source>,
    target: ToneTarget,
    empty_label: &str,
) -> Option<Tone3000Request> {
    let palette = Palette::of(ui.ctx());
    let path = match path {
        None => {
            ui.label(RichText::new(empty_label).color(palette.text_dim).italics());
            return None;
        }
        Some(path) => path,
    };
    if std::path::Path::new(path).exists() {
        ui.add(egui::Label::new(RichText::new(file_name(path)).small()).truncate()).on_hover_text(path);
        return None;
    }
    ui.colored_label(palette.danger, format!("File not found: {}", file_name(path)))
        .on_hover_text(path);
    let source = source?;
    ui.button("Download again from TONE3000")
        .clicked()
        .then_some(Tone3000Request::Reload {
            target,
            tone_id: source.tone_id,
            model_id: Some(source.model_id),
        })
}

fn cabinet_ui(ui: &mut Ui, params: &mut CabinetParams, color: Color32) -> (bool, Option<Tone3000Request>) {
    let mut changed = false;
    let mut request = None;

    if let Some(source) = &params.source {
        source_block(ui, source);
    }

    ui.vertical(|ui| {
        // `rfd` (native file picker) has no Android backend, so Android
        // falls back to a plain text field below.
        #[cfg(not(target_os = "android"))]
        if ui.button("Load IR...").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("WAV", &["wav"])
                .pick_file()
        {
            params.ir_path = Some(path.display().to_string());
            params.source = None;
            changed = true;
        }
        if browse_button(ui) {
            request = Some(Tone3000Request::Browse(ToneTarget::Ir));
        }
    });

    #[cfg(target_os = "android")]
    {
        let mut path = params.ir_path.clone().unwrap_or_default();
        ui.label("IR file path:");
        if ui.text_edit_singleline(&mut path).changed() {
            params.ir_path = if path.is_empty() { None } else { Some(path) };
            params.source = None;
            changed = true;
        }
    }

    request = request.or(loaded_file_ui(
        ui,
        params.ir_path.as_deref(),
        params.source.as_ref(),
        ToneTarget::Ir,
        "No IR loaded",
    ));

    let d = CabinetParams::default();
    let k = K { color };
    changed |= knobs(ui, |ui| k.unit(ui, &mut params.mix, "Mix", d.mix));

    (changed, request)
}

fn nam_ui(ui: &mut Ui, params: &mut NamParams, color: Color32) -> (bool, Option<Tone3000Request>) {
    let palette = Palette::of(ui.ctx());
    let mut changed = false;
    let mut request = None;

    if let Some(source) = &params.source {
        source_block(ui, source);
    }

    ui.vertical(|ui| {
        #[cfg(not(target_os = "android"))]
        if ui.button("Load .nam...").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("NAM model", &["nam"])
                .pick_file()
        {
            params.model_path = Some(path.display().to_string());
            params.source = None;
            changed = true;
        }
        if browse_button(ui) {
            request = Some(Tone3000Request::Browse(ToneTarget::Nam));
        }
    });

    #[cfg(target_os = "android")]
    {
        let mut path = params.model_path.clone().unwrap_or_default();
        ui.label(".nam file path:");
        if ui.text_edit_singleline(&mut path).changed() {
            params.model_path = if path.is_empty() { None } else { Some(path) };
            params.source = None;
            changed = true;
        }
    }

    request = request.or(loaded_file_ui(
        ui,
        params.model_path.as_deref(),
        params.source.as_ref(),
        ToneTarget::Nam,
        "No model loaded",
    ));

    // Filled in by `nam::preload`, which the app runs before sending a
    // chain to the audio thread.
    if let Some(path) = params.model_path.as_deref() {
        match nam::cached_info(path) {
            Some(Ok(info)) => {
                let name = info.name.clone().unwrap_or_else(|| file_name(path));
                ui.add(egui::Label::new(RichText::new(format!("{name} ({})", info.architecture)).small()).truncate());
                if let Some(warning) = nam_warning(&info) {
                    ui.colored_label(palette.warn, warning);
                }
            }
            Some(Err(error)) if std::path::Path::new(path).exists() => {
                ui.colored_label(palette.danger, error);
            }
            _ => {}
        }
    }

    let d = NamParams::default();
    let k = K { color };
    changed |= knobs(ui, |ui| {
        let mut c = k.db(ui, &mut params.input_gain_db, -24.0..=24.0, "Input", d.input_gain_db);
        c |= k.db(ui, &mut params.output_gain_db, -24.0..=24.0, "Output", d.output_gain_db);
        c
    });
    changed |= ui
        .checkbox(&mut params.normalize, "Normalize loudness")
        .on_hover_text("Play every model at a similar level, using the loudness stored in the file")
        .changed();

    (changed, request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn every_pedal_card_renders() {
        let ctx = egui::Context::default();
        let mut chain: Vec<PedalInstance> = PedalDescription::iter().map(PedalInstance::new).collect();
        let count = chain.len();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for (i, pedal) in chain.iter_mut().enumerate() {
                    let card = pedal_card(ui, i, count, pedal, style::PEDAL_CARD_WIDTH);
                    assert_eq!(card.action, PedalAction::None);
                }
            });
        });
    }

    #[test]
    fn reset_restores_defaults_but_keeps_files() {
        let mut nam = PedalDescription::Nam(NamParams {
            model_path: Some("/m.nam".into()),
            input_gain_db: 6.0,
            ..Default::default()
        });
        reset_to_defaults(&mut nam);
        match nam {
            PedalDescription::Nam(p) => {
                assert_eq!(p.model_path.as_deref(), Some("/m.nam"));
                assert_eq!(p.input_gain_db, 0.0);
            }
            _ => unreachable!(),
        }
        let mut delay = PedalDescription::Delay(DelayParams { delay_ms: 10.0, ..Default::default() });
        reset_to_defaults(&mut delay);
        assert_eq!(delay, PedalDescription::Delay(DelayParams::default()));
    }

    #[test]
    fn looper_cycle_goes_record_play_overdub() {
        let mut c = LooperCommand::Idle;
        let mut seen = Vec::new();
        for _ in 0..4 {
            c = looper_cycle(c);
            seen.push(c);
        }
        assert_eq!(
            seen,
            vec![LooperCommand::Record, LooperCommand::Play, LooperCommand::Overdub, LooperCommand::Play]
        );
    }
}
