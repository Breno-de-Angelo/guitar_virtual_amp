use eframe::egui::{Slider, Ui};

use crate::shared::pedals::{
    AmpParams, ChorusParams, CompressorParams, DelayParams, DistortionParams, EqParams,
    FlangerParams, LowPassParams, NoiseGateParams, PedalDescription, PhaserParams, ReverbParams,
    TremoloParams, WahWahParams,
};

pub enum PedalAction {
    None,
    Updated,
    Deleted,
}

/// Returns true if component has changed
pub fn render_pedal_ui(ui: &mut Ui, pedal: &mut PedalDescription) -> PedalAction {
    ui.group(|ui| {
        if ui.button("Delete").clicked() {
            PedalAction::Deleted
        } else {
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
            };
            if updated {
                PedalAction::Updated
            } else {
                PedalAction::None
            }
        }
    })
    .inner
}

fn amp_ui(ui: &mut Ui, params: &mut AmpParams) -> bool {
    ui.group(|ui| {
        ui.label("Amp");
        let slider = ui.add(Slider::new(&mut params.gain, 0.0..=5.0).text("Gain"));
        slider.changed()
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
