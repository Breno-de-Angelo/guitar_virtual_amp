use eframe::egui::{Slider, Ui};

use crate::shared::pedals::{
    AmpParams, DelayParams, FlangerParams, LowPassParams, PedalDescription, ReverbParams,
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

fn delay_ui(ui: &mut Ui, params: &mut DelayParams) -> bool {
    ui.group(|ui| {
        ui.label("Delay");
        let delay_slider = ui.add(Slider::new(&mut params.delay, 0.0..=1.0).text("Delay (s)"));
        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=1.0).text("Gain"));
        delay_slider.changed() || gain_slider.changed()
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
        ui.label("Low Pass");
        let delay_range_slider =
            ui.add(Slider::new(&mut params.delay_range, 0.0..=10.0).text("Delay Range (ms)"));
        let delay_rate_slider =
            ui.add(Slider::new(&mut params.delay_rate, 0.0..=5.0).text("Delay Rate (Hz)"));
        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=1.0).text("Gain"));
        delay_range_slider.changed() || delay_rate_slider.changed() || gain_slider.changed()
    })
    .inner
}
