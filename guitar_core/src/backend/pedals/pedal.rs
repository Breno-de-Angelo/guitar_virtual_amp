use crate::{
    backend::pedals::{
        amp::Amp, delay::Delay, distortion::Distortion, flanger::Flanger, looper::Looper,
        low_pass::LowPass, reverb::Reverb, wah_wah::WahWah,
    },
    shared::pedals::PedalDescription,
};

pub trait Pedal: Send {
    fn apply_effect(&mut self, input: i16) -> i16;
}

pub struct PedalChain {
    pedals: Vec<Box<dyn Pedal>>,
}

impl PedalChain {
    pub fn new() -> Self {
        PedalChain { pedals: Vec::new() }
    }

    pub fn from_description(description: Vec<PedalDescription>) -> Self {
        PedalChain {
            pedals: description
                .iter()
                .map(|&pedal_description| match pedal_description {
                    PedalDescription::Reverb(params) => {
                        Box::new(Reverb::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Amp(params) => Box::new(Amp::new(params)) as Box<dyn Pedal>,
                    PedalDescription::LowPass(params) => {
                        Box::new(LowPass::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Delay(params) => {
                        Box::new(Delay::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Flanger(params) => {
                        Box::new(Flanger::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::WahWah(params) => {
                        Box::new(WahWah::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Distortion(params) => {
                        Box::new(Distortion::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Looper(params) => {
                        Box::new(Looper::new(params)) as Box<dyn Pedal>
                    }
                })
                .collect(),
        }
    }

    pub fn process_sample(&mut self, mut sample: i16) -> i16 {
        for pedal in &mut self.pedals {
            sample = pedal.apply_effect(sample);
        }
        sample
    }
}
