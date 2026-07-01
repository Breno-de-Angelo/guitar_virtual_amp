use crate::backend::ring_buffer::RingBuffer;

pub struct Allpass {
    buf: RingBuffer<65536, f32>,
    delay_len: usize,
    feedback: f32,
}

impl Allpass {
    pub fn new(delay_len: usize, feedback: f32) -> Self {
        Self {
            buf: RingBuffer::new(),
            delay_len: delay_len.max(1),
            feedback,
        }
    }

    pub fn process(&mut self, input: f32) -> f32 {
        let delayed = if self.buf.len() > self.delay_len {
            self.buf[self.buf.len() - 1 - self.delay_len]
        } else {
            0.0
        };

        let output = delayed - self.feedback * input;
        let into_delay = input + self.feedback * output;
        self.buf.push(into_delay);

        output
    }
}
