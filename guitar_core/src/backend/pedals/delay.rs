use crate::{
    backend::{pedals::pedal::Pedal, ring_buffer::RingBuffer},
    shared::{config::GLOBAL_CONFIG, pedals::DelayParams},
};

pub struct Delay {
    buffer: RingBuffer<f32>,
    params: DelayParams,
    delay_samples: usize,
    last_feedback: f32,
}

impl Delay {
    pub fn new(params: DelayParams) -> Self {
        let delay_samples = ((params.delay_ms / 1000.0) * GLOBAL_CONFIG.sample_rate)
            .round()
            .max(1.0) as usize;

        Self {
            buffer: RingBuffer::new(65536),
            params,
            delay_samples,
            last_feedback: 0.0,
        }
    }
}

impl Pedal for Delay {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32;

        // Multi-tap: somar taps com múltiplos do delay principal
        let mut delayed_sum = 0.0_f32;
        for tap in 1..=self.params.taps {
            let tap_delay = self.delay_samples * tap;
            if self.buffer.len() >= tap_delay {
                delayed_sum += self.buffer[self.buffer.len() - tap_delay];
            }
        }
        delayed_sum *= self.params.gain / self.params.taps as f32;

        // Feedback com filtro passa-baixa (damping)
        let feedback_sample = delayed_sum * self.params.feedback;
        let filtered_feedback = self.last_feedback * (1.0 - self.params.damping)
            + feedback_sample * self.params.damping;
        self.last_feedback = filtered_feedback;

        // Escrever no buffer
        self.buffer.push(input_f + filtered_feedback);

        // Mix wet/dry
        let wet = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;
        let out_f = input_f * dry + delayed_sum * wet;

        out_f.clamp(-32768.0, 32767.0) as i16
    }
}
