use eframe::egui::{Color32, ComboBox, Slider, Ui};

use crate::backend::pedals::nam;
use crate::frontend::ui::tone3000::{
    Tone3000Request, ToneTarget, browse_button, nam_warning, source_block,
};
use crate::shared::pedals::{
    AmpParams, AmpVoicing, CabinetParams, ChorusParams, CompressorParams, DelayParams,
    DistortionParams, EqParams, FlangerParams, LooperCommand, LooperParams, LowPassParams,
    NamParams, NoiseGateParams, OctaverParams, PedalDescription, PedalInstance, PhaserParams,
    PitchShifterParams, ReverbParams, TremoloParams, Tone3000Source, WahWahParams,
};

pub enum PedalAction {
    None,
    Updated,
    Deleted,
    /// The pedal wants the TONE3000 window (browse, or re-download a tone).
    Tone3000(Tone3000Request),
}

/// Returns the action taken this frame (bypass toggle and per-param sliders
/// both report `Updated`, so the caller pushes a chain update either way).
pub fn render_pedal_ui(ui: &mut Ui, instance: &mut PedalInstance) -> PedalAction {
    ui.group(|ui| {
        let mut changed = false;
        let mut deleted = false;

        ui.horizontal(|ui| {
            // Bypass LED/footswitch: green when the pedal is active, gray when
            // bypassed. Clicking toggles it without removing the pedal from
            // the chain (real pedalboard "stomp the switch" workflow).
            let led_color = if instance.enabled {
                eframe::egui::Color32::from_rgb(60, 220, 60)
            } else {
                eframe::egui::Color32::GRAY
            };
            let (rect, response) = ui.allocate_exact_size(
                eframe::egui::vec2(16.0, 16.0),
                eframe::egui::Sense::click(),
            );
            ui.painter()
                .circle_filled(rect.center(), 7.0, led_color);
            if response
                .on_hover_text("Bypass this pedal")
                .clicked()
            {
                instance.enabled = !instance.enabled;
                changed = true;
            }

            if ui.button("Delete").clicked() {
                deleted = true;
            }
        });

        if deleted {
            return PedalAction::Deleted;
        }

        let pedal = &mut instance.description;
        let mut request = None;
        let updated = match pedal {
            PedalDescription::Amp(params) => amp_ui(ui, params),
            PedalDescription::Delay(params) => delay_ui(ui, params),
            PedalDescription::Reverb(params) => reverb_ui(ui, params),
            PedalDescription::LowPass(params) => low_pass_ui(ui, params),
            PedalDescription::Flanger(params) => flanger_ui(ui, params),
            PedalDescription::WahWah(params) => wah_wah_ui(ui, params),
            PedalDescription::Distortion(params) => distortion_ui(ui, params),
            PedalDescription::NoiseGate(params) => noise_gate_ui(ui, params),
            PedalDescription::Compressor(params) => compressor_ui(ui, params),
            PedalDescription::Tremolo(params) => tremolo_ui(ui, params),
            PedalDescription::Chorus(params) => chorus_ui(ui, params),
            PedalDescription::Eq(params) => eq_ui(ui, params),
            PedalDescription::Phaser(params) => phaser_ui(ui, params),
            PedalDescription::Octaver(params) => octaver_ui(ui, params),
            PedalDescription::PitchShift(params) => pitch_shifter_ui(ui, params),
            PedalDescription::Looper(params) => looper_ui(ui, params),
            PedalDescription::Cabinet(params) => {
                let (updated, cabinet_request) = cabinet_ui(ui, params);
                request = cabinet_request;
                updated
            }
            PedalDescription::Nam(params) => {
                let (updated, nam_request) = nam_ui(ui, params);
                request = nam_request;
                updated
            }
        };
        changed |= updated;

        if let Some(request) = request {
            PedalAction::Tone3000(request)
        } else if changed {
            PedalAction::Updated
        } else {
            PedalAction::None
        }
    })
    .inner
}

fn amp_ui(ui: &mut Ui, params: &mut AmpParams) -> bool {
    ui.group(|ui| {
        ui.label("Amp");

        let mut changed = false;

        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=5.0).text("Gain"));
        changed |= gain_slider.changed();

        let voicing_response = ComboBox::from_label("Voicing")
            .selected_text(format!("{:?}", params.voicing))
            .show_ui(ui, |ui| {
                let mut voicing_changed = false;
                voicing_changed |= ui
                    .selectable_value(&mut params.voicing, AmpVoicing::Clean, "Clean")
                    .changed();
                voicing_changed |= ui
                    .selectable_value(&mut params.voicing, AmpVoicing::Crunch, "Crunch")
                    .changed();
                voicing_changed |= ui
                    .selectable_value(&mut params.voicing, AmpVoicing::Lead, "Lead")
                    .changed();
                voicing_changed
            });
        changed |= voicing_response.inner.unwrap_or(false);

        let bass_slider = ui.add(Slider::new(&mut params.bass, -12.0..=12.0).text("Bass (dB)"));
        let mid_slider = ui.add(Slider::new(&mut params.mid, -12.0..=12.0).text("Mid (dB)"));
        let treble_slider =
            ui.add(Slider::new(&mut params.treble, -12.0..=12.0).text("Treble (dB)"));
        changed |= bass_slider.changed();
        changed |= mid_slider.changed();
        changed |= treble_slider.changed();

        changed
    })
    .inner
}

pub fn delay_ui(ui: &mut Ui, params: &mut DelayParams) -> bool {
    ui.group(|ui| {
        ui.label("Delay");

        let mut changed = false;

        let delay_slider =
            ui.add(Slider::new(&mut params.delay_ms, 1.0..=2000.0).text("Delay (ms)"));
        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=1.0).text("Gain"));
        let feedback_slider =
            ui.add(Slider::new(&mut params.feedback, 0.0..=0.99).text("Feedback"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));
        let taps_slider = ui.add(Slider::new(&mut params.taps, 1..=8).text("Taps"));
        let damping_slider = ui.add(Slider::new(&mut params.damping, 0.0..=0.99).text("Damping"));

        changed |= delay_slider.changed();
        changed |= gain_slider.changed();
        changed |= feedback_slider.changed();
        changed |= mix_slider.changed();
        changed |= taps_slider.changed();
        changed |= damping_slider.changed();

        changed
    })
    .inner
}

pub fn reverb_ui(ui: &mut Ui, params: &mut ReverbParams) -> bool {
    ui.group(|ui| {
        ui.label("Reverb");

        let mut changed = false;

        let room_slider = ui.add(Slider::new(&mut params.room_size, 0.0..=0.99).text("Room Size"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));
        let damping_slider = ui.add(Slider::new(&mut params.damping, 0.0..=0.99).text("Damping"));

        changed |= room_slider.changed();
        changed |= mix_slider.changed();
        changed |= damping_slider.changed();

        changed
    })
    .inner
}

fn low_pass_ui(ui: &mut Ui, params: &mut LowPassParams) -> bool {
    ui.group(|ui| {
        ui.label("Low Pass");
        let slider =
            ui.add(Slider::new(&mut params.frequency, 50.0..=10_000.0).text("Frequency (Hz)"));
        slider.changed()
    })
    .inner
}

fn flanger_ui(ui: &mut Ui, params: &mut FlangerParams) -> bool {
    ui.group(|ui| {
        ui.label("Flanger");
        let delay_range_slider =
            ui.add(Slider::new(&mut params.delay_range, 0.0..=10.0).text("Delay Range (ms)"));
        let delay_rate_slider =
            ui.add(Slider::new(&mut params.delay_rate, 0.0..=5.0).text("Delay Rate (Hz)"));
        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=1.0).text("Gain"));
        delay_range_slider.changed() || delay_rate_slider.changed() || gain_slider.changed()
    })
    .inner
}

fn wah_wah_ui(ui: &mut Ui, params: &mut WahWahParams) -> bool {
    ui.group(|ui| {
        ui.label("WahWah");

        let mut changed = false;

        let frequency_slider = ui.add(Slider::new(&mut params.frequency, 100.0..=8000.0).text("Frequency (Hz)"));
        let resonance_slider = ui.add(Slider::new(&mut params.resonance, 0.1..=10.0).text("Resonance"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));
        let lfo_rate_slider = ui.add(Slider::new(&mut params.lfo_rate, 0.1..=10.0).text("LFO Rate (Hz)"));
        let lfo_depth_slider = ui.add(Slider::new(&mut params.lfo_depth, 0.0..=1.0).text("LFO Depth"));

        changed |= frequency_slider.changed();
        changed |= resonance_slider.changed();
        changed |= mix_slider.changed();
        changed |= lfo_rate_slider.changed();
        changed |= lfo_depth_slider.changed();

        changed
    })
    .inner
}

fn pitch_shifter_ui(ui: &mut Ui, params: &mut PitchShifterParams) -> bool {
    ui.group(|ui| {
        ui.label("Pitch Shifter");

        let mut changed = false;

        let semitones_slider =
            ui.add(Slider::new(&mut params.semitones, -12.0..=12.0).text("Semitones"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));

        changed |= semitones_slider.changed();
        changed |= mix_slider.changed();

        changed
    })
    .inner
}

fn distortion_ui(ui: &mut Ui, params: &mut DistortionParams) -> bool {
    ui.group(|ui| {
        ui.label("Distortion");

        let mut changed = false;

        let drive_slider = ui.add(Slider::new(&mut params.drive, 0.0..=10.0).text("Drive"));
        let tone_slider = ui.add(Slider::new(&mut params.tone, 0.0..=1.0).text("Tone"));
        let level_slider = ui.add(Slider::new(&mut params.level, 0.0..=2.0).text("Level"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));
        let bias_slider = ui.add(Slider::new(&mut params.bias, -1.0..=1.0).text("Bias"));

        changed |= drive_slider.changed();
        changed |= tone_slider.changed();
        changed |= level_slider.changed();
        changed |= mix_slider.changed();
        changed |= bias_slider.changed();

        changed
    })
    .inner
}

fn noise_gate_ui(ui: &mut Ui, params: &mut NoiseGateParams) -> bool {
    ui.group(|ui| {
        ui.label("Noise Gate");

        let mut changed = false;

        let threshold_slider =
            ui.add(Slider::new(&mut params.threshold, 0.0..=0.5).text("Threshold"));
        let attack_slider =
            ui.add(Slider::new(&mut params.attack_ms, 0.1..=50.0).text("Attack (ms)"));
        let release_slider =
            ui.add(Slider::new(&mut params.release_ms, 1.0..=1000.0).text("Release (ms)"));
        let hold_slider = ui.add(Slider::new(&mut params.hold_ms, 0.0..=500.0).text("Hold (ms)"));

        changed |= threshold_slider.changed();
        changed |= attack_slider.changed();
        changed |= release_slider.changed();
        changed |= hold_slider.changed();

        changed
    })
    .inner
}

fn compressor_ui(ui: &mut Ui, params: &mut CompressorParams) -> bool {
    ui.group(|ui| {
        ui.label("Compressor");

        let mut changed = false;

        let threshold_slider =
            ui.add(Slider::new(&mut params.threshold, 0.0..=1.0).text("Threshold"));
        let ratio_slider = ui.add(Slider::new(&mut params.ratio, 1.0..=20.0).text("Ratio"));
        let attack_slider =
            ui.add(Slider::new(&mut params.attack_ms, 0.1..=100.0).text("Attack (ms)"));
        let release_slider =
            ui.add(Slider::new(&mut params.release_ms, 1.0..=1000.0).text("Release (ms)"));
        let makeup_gain_slider =
            ui.add(Slider::new(&mut params.makeup_gain, 0.0..=5.0).text("Makeup Gain"));

        changed |= threshold_slider.changed();
        changed |= ratio_slider.changed();
        changed |= attack_slider.changed();
        changed |= release_slider.changed();
        changed |= makeup_gain_slider.changed();

        changed
    })
    .inner
}

fn tremolo_ui(ui: &mut Ui, params: &mut TremoloParams) -> bool {
    ui.group(|ui| {
        ui.label("Tremolo");

        let mut changed = false;

        let rate_slider = ui.add(Slider::new(&mut params.rate_hz, 0.5..=10.0).text("Rate (Hz)"));
        let depth_slider = ui.add(Slider::new(&mut params.depth, 0.0..=1.0).text("Depth"));

        changed |= rate_slider.changed();
        changed |= depth_slider.changed();

        changed
    })
    .inner
}

fn chorus_ui(ui: &mut Ui, params: &mut ChorusParams) -> bool {
    ui.group(|ui| {
        ui.label("Chorus");

        let mut changed = false;

        let delay_slider = ui.add(Slider::new(&mut params.delay_ms, 5.0..=40.0).text("Delay (ms)"));
        let depth_slider = ui.add(Slider::new(&mut params.depth_ms, 0.0..=15.0).text("Depth (ms)"));
        let rate_slider = ui.add(Slider::new(&mut params.rate_hz, 0.1..=5.0).text("Rate (Hz)"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));

        changed |= delay_slider.changed();
        changed |= depth_slider.changed();
        changed |= rate_slider.changed();
        changed |= mix_slider.changed();

        changed
    })
    .inner
}

fn eq_ui(ui: &mut Ui, params: &mut EqParams) -> bool {
    ui.group(|ui| {
        ui.label("EQ");

        let mut changed = false;

        let low_slider = ui.add(Slider::new(&mut params.low_gain_db, -12.0..=12.0).text("Low (dB)"));
        let mid_slider = ui.add(Slider::new(&mut params.mid_gain_db, -12.0..=12.0).text("Mid (dB)"));
        let high_slider = ui.add(Slider::new(&mut params.high_gain_db, -12.0..=12.0).text("High (dB)"));

        changed |= low_slider.changed();
        changed |= mid_slider.changed();
        changed |= high_slider.changed();

        changed
    })
    .inner
}

fn phaser_ui(ui: &mut Ui, params: &mut PhaserParams) -> bool {
    ui.group(|ui| {
        ui.label("Phaser");

        let mut changed = false;

        let rate_slider = ui.add(Slider::new(&mut params.rate_hz, 0.05..=5.0).text("Rate (Hz)"));
        let depth_slider = ui.add(Slider::new(&mut params.depth, 0.0..=1.0).text("Depth"));
        let feedback_slider = ui.add(Slider::new(&mut params.feedback, 0.0..=0.95).text("Feedback"));
        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));

        changed |= rate_slider.changed();
        changed |= depth_slider.changed();
        changed |= feedback_slider.changed();
        changed |= mix_slider.changed();

        changed
    })
    .inner
}

fn octaver_ui(ui: &mut Ui, params: &mut OctaverParams) -> bool {
    ui.group(|ui| {
        ui.label("Octaver");

        let mut changed = false;

        let octave_mix_slider =
            ui.add(Slider::new(&mut params.octave_down_mix, 0.0..=1.0).text("Octave Mix"));
        let dry_mix_slider = ui.add(Slider::new(&mut params.dry_mix, 0.0..=1.0).text("Dry Mix"));
        let tone_slider = ui.add(Slider::new(&mut params.tone, 0.0..=1.0).text("Tone"));

        changed |= octave_mix_slider.changed();
        changed |= dry_mix_slider.changed();
        changed |= tone_slider.changed();

        changed
    })
    .inner
}

fn looper_ui(ui: &mut Ui, params: &mut LooperParams) -> bool {
    ui.group(|ui| {
        ui.label("Looper");

        let mut changed = false;

        let status = match params.command {
            LooperCommand::Idle => "Idle",
            LooperCommand::Record => "Recording",
            LooperCommand::Play => "Playing",
            LooperCommand::Overdub => "Overdubbing",
            LooperCommand::Stop => "Stopped",
            LooperCommand::Clear => "Idle",
        };
        ui.label(format!("Status: {status}"));

        ui.horizontal(|ui| {
            if ui.button("Record").clicked() {
                params.command = LooperCommand::Record;
                changed = true;
            }
            if ui.button("Play").clicked() {
                params.command = LooperCommand::Play;
                changed = true;
            }
            if ui.button("Overdub").clicked() {
                params.command = LooperCommand::Overdub;
                changed = true;
            }
            if ui.button("Stop").clicked() {
                params.command = LooperCommand::Stop;
                changed = true;
            }
            if ui.button("Clear").clicked() {
                params.command = LooperCommand::Clear;
                changed = true;
            }
        });

        let max_loop_slider = ui.add(
            Slider::new(&mut params.max_loop_seconds, 1.0..=60.0).text("Max Loop Length (s)"),
        );
        changed |= max_loop_slider.changed();

        changed
    })
    .inner
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
    let path = match path {
        None => {
            ui.label(empty_label);
            return None;
        }
        Some(path) => path,
    };
    if std::path::Path::new(path).exists() {
        ui.label(file_name(path)).on_hover_text(path);
        return None;
    }
    ui.colored_label(Color32::RED, format!("File not found: {}", file_name(path)))
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

fn cabinet_ui(ui: &mut Ui, params: &mut CabinetParams) -> (bool, Option<Tone3000Request>) {
    ui.group(|ui| {
        ui.label("Cabinet");

        let mut changed = false;
        let mut request = None;

        if let Some(source) = &params.source {
            source_block(ui, source);
        }

        ui.horizontal(|ui| {
            // `rfd` (native file picker) has no Android backend, so Android
            // falls back to a plain text field below.
            #[cfg(not(target_os = "android"))]
            if ui.button("Load IR...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("WAV", &["wav"])
                    .pick_file()
                {
                    params.ir_path = Some(path.display().to_string());
                    params.source = None;
                    changed = true;
                }
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

        let mix_slider = ui.add(Slider::new(&mut params.mix, 0.0..=1.0).text("Mix"));
        changed |= mix_slider.changed();

        (changed, request)
    })
    .inner
}

fn nam_ui(ui: &mut Ui, params: &mut NamParams) -> (bool, Option<Tone3000Request>) {
    ui.group(|ui| {
        ui.label("NAM Model");

        let mut changed = false;
        let mut request = None;

        if let Some(source) = &params.source {
            source_block(ui, source);
        }

        ui.horizontal(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button("Load .nam...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("NAM model", &["nam"])
                    .pick_file()
                {
                    params.model_path = Some(path.display().to_string());
                    params.source = None;
                    changed = true;
                }
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
                    ui.label(format!("{name} ({})", info.architecture));
                    if let Some(warning) = nam_warning(&info) {
                        ui.colored_label(Color32::YELLOW, warning);
                    }
                }
                Some(Err(error)) if std::path::Path::new(path).exists() => {
                    ui.colored_label(Color32::RED, error);
                }
                _ => {}
            }
        }

        changed |= ui
            .add(Slider::new(&mut params.input_gain_db, -24.0..=24.0).text("Input (dB)"))
            .changed();
        changed |= ui
            .add(Slider::new(&mut params.output_gain_db, -24.0..=24.0).text("Output (dB)"))
            .changed();
        changed |= ui
            .checkbox(&mut params.normalize, "Normalize loudness")
            .on_hover_text("Play every model at a similar level, using the loudness stored in the file")
            .changed();

        (changed, request)
    })
    .inner
}
