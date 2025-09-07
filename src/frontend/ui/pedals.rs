use eframe::egui::{Slider, Ui};

use crate::shared::pedals::{
    AmpParams, DelayParams, LowPassParams, PedalDescription, ReverbParams,
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
        let delay_slider = ui.add(Slider::new(&mut params.delay, 0.0..=1.0).text("Delay"));
        let gain_slider = ui.add(Slider::new(&mut params.gain, 0.0..=1.0).text("Gain"));
        delay_slider.changed() || gain_slider.changed()
    })
    .inner
}

fn reverb_ui(ui: &mut Ui, params: &mut ReverbParams) -> bool {
    ui.group(|ui| {
        ui.label("Reverb");
        let slider = ui.add(Slider::new(&mut params.feedback, 0.0..=1.0).text("Feedback"));
        slider.changed()
    })
    .inner
}

fn low_pass_ui(ui: &mut Ui, params: &mut LowPassParams) -> bool {
    ui.group(|ui| {
        ui.label("Low Pass");
        let slider = ui.add(Slider::new(&mut params.frequency, 50.0..=10_000.0).text("Frequency"));
        slider.changed()
    })
    .inner
}
