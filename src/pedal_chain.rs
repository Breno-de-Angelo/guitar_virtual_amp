use crate::ring_buffer;

// struct Pedal {
//     input: circular_array::CircularArray<BUFFER_SIZE, i16>,
//     output: circular_array::CircularArray<BUFFER_SIZE, i16>,
// }

// pub struct PedalChain {
//     pedals: Vec<Pedal>,
// }

pub trait Pedal {
    fn apply_effect(&self, input: i16) -> i16;
}

pub struct PedalChain {
    pedals: Vec<Box<dyn Pedal>>,
}

impl PedalChain {
    pub fn new() -> Self {
        PedalChain { pedals: Vec::new() }
    }

    pub fn add_pedal(&mut self, pedal: Box<dyn Pedal>) {
        self.pedals.push(pedal);
    }

    pub fn process_sample(&self, mut sample: i16) -> i16 {
        for pedal in &self.pedals {
            sample = pedal.apply_effect(sample);
        }
        sample
    }
}

pub struct Reverb {
    buffer: ring_buffer::RingBuffer<32768, i16>,
    feedback: f32,
}

impl Reverb {
    pub fn new(feedback: f32) -> Self {
        Self {
            buffer: ring_buffer::RingBuffer::new(),
            feedback,
        }
    }
}

impl Pedal for Reverb {
    fn apply_effect(&self, input: i16) -> i16 {
        let delayed = *self.buffer.iter().rev().nth(32767).unwrap_or(&0);
        let output =
            (input as f32 + delayed as f32 * self.feedback).clamp(-32768.0, 32767.0) as i16;
        output
    }
}

pub struct Amp {
    gain: f32,
}

impl Amp {
    pub fn new(gain: f32) -> Self {
        Self { gain }
    }
}

impl Pedal for Amp {
    fn apply_effect(&self, input: i16) -> i16 {
        let output = (input as f32 * self.gain).clamp(-32768.0, 32767.0) as i16;
        output
    }
}
