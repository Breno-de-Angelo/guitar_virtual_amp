use std::f32::consts::PI;

use crate::{
    backend::{pedals::pedal::Pedal, ring_buffer::RingBuffer},
    shared::{config::GLOBAL_CONFIG, pedals::FlangerParams},
};

pub struct Flanger {
    buffer: RingBuffer<65536, i16>,
    flange_angle: f32,
    omega: f32,
    params: FlangerParams,
}

impl Flanger {
    pub fn new(params: FlangerParams) -> Self {
        Self {
            buffer: RingBuffer::new(),
            flange_angle: 0.0,
            omega: 2.0 * PI * params.delay_rate,
            params,
        }
    }
}

impl Pedal for Flanger {
    fn apply_effect(&mut self, input: i16) -> i16 {
        self.buffer.push(input);
        let delay_samples = (self.params.delay_range * GLOBAL_CONFIG.sample_rate / 1000.0 / 2.0
            * (1.0 - f32::cos(self.flange_angle)))
        .round() as usize;

        let delayed = if self.buffer.len() > delay_samples {
            self.buffer[self.buffer.len() - 1 - delay_samples]
        } else {
            0
        };
        let output =
            (input as f32 + delayed as f32 * self.params.gain).clamp(-32768.0, 32767.0) as i16;
        self.flange_angle += self.omega / GLOBAL_CONFIG.sample_rate;
        if self.flange_angle >= 2.0 * PI {
            self.flange_angle -= 2.0 * PI;
        }
        output
    }
}
