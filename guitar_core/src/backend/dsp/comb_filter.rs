use crate::backend::ring_buffer::RingBuffer;

pub struct CombFilter {
    buf: RingBuffer<f32>,
    delay_len: usize,
    feedback: f32,
    damp1: f32,
    damp2: f32,
    filter_store: f32,
}

impl CombFilter {
    pub fn new(delay_len: usize, feedback: f32, damping: f32) -> Self {
        Self {
            buf: RingBuffer::new(65536),
            delay_len: delay_len.max(1),
            feedback,
            damp1: damping.clamp(0.0, 0.999),
            damp2: 1.0 - damping.clamp(0.0, 0.999),
            filter_store: 0.0,
        }
    }

    pub fn process(&mut self, input: f32) -> f32 {
        let delayed = if self.buf.len() > self.delay_len {
            self.buf[self.buf.len() - 1 - self.delay_len]
        } else {
            0.0
        };

        self.filter_store = delayed * self.damp2 + self.filter_store * self.damp1;

        let output = delayed;
        let into_delay = input + self.filter_store * self.feedback;
        self.buf.push(into_delay);

        output
    }
}
