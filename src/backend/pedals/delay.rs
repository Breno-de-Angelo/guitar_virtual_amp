use crate::{
    backend::{pedals::pedal::Pedal, ring_buffer::RingBuffer},
    shared::{config::GLOBAL_CONFIG, pedals::DelayParams},
};

pub struct Delay {
    buffer: RingBuffer<65536, i16>,
    delay_samples: usize,
    params: DelayParams,
}

impl Delay {
    pub fn new(params: DelayParams) -> Self {
        Self {
            buffer: RingBuffer::new(),
            delay_samples: (params.delay * GLOBAL_CONFIG.sample_rate) as usize,
            params,
        }
    }
}

impl Pedal for Delay {
    fn apply_effect(&mut self, input: i16) -> i16 {
        self.buffer.push(input);
        let delayed = if self.buffer.len() >= self.delay_samples {
            self.buffer[self.buffer.len() - self.delay_samples]
        } else {
            0
        };
        let output =
            (input as f32 + delayed as f32 * self.params.gain).clamp(-32768.0, 32767.0) as i16;
        output
    }
}
