use std::f32::consts::{E, PI};

use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::LowPassParams},
};

pub struct LowPass {
    last_sample: f32,
    alpha: f32,
}

impl LowPass {
    pub fn new(params: LowPassParams) -> Self {
        Self {
            last_sample: 0.0,
            alpha: f32::powf(E, -2.0 * PI * params.frequency / GLOBAL_CONFIG.sample_rate),
        }
    }
}

impl Pedal for LowPass {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let sample = self.last_sample * self.alpha + input as f32 * (1.0 - self.alpha);
        let output = sample.clamp(-32768.0, 32767.0) as i16;
        output
    }
}
