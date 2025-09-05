use crate::ring_buffer;

const BUFFER_SIZE: usize = 16;

// struct Pedal {
//     input: circular_array::CircularArray<BUFFER_SIZE, i16>,
//     output: circular_array::CircularArray<BUFFER_SIZE, i16>,
// }

// pub struct PedalChain {
//     pedals: Vec<Pedal>,
// }

// Trait para pedals
trait Pedal {
    fn apply_effect(&mut self, input: i16) -> i16;
}

struct PedalChain {
    pedals: Vec<Box<dyn Pedal>>,
}

impl PedalChain {
    fn new() -> Self {
        PedalChain { pedals: Vec::new() }
    }

    fn add_pedal(&mut self, pedal: Box<dyn Pedal>) {
        self.pedals.push(pedal);
    }

    fn process_sample(&mut self, mut sample: i16) -> i16 {
        for pedal in &mut self.pedals {
            sample = pedal.apply_effect(sample);
        }
        sample
    }
}

// Implementação de um Reverb simples
struct Reverb {
    buffer: ring_buffer::RingBuffer<BUFFER_SIZE, i16>,
    feedback: f32,
}

impl Reverb {
    fn new(feedback: f32) -> Self {
        Self {
            buffer: ring_buffer::RingBuffer::new(),
            feedback,
        }
    }
}

impl Pedal for Reverb {
    fn apply_effect(&mut self, input: i16) -> i16 {
        // lê o sample anterior do buffer
        let delayed = *self.buffer.iter().rev().nth(4).unwrap_or(&0);
        let output =
            (input as f32 + delayed as f32 * self.feedback).clamp(-32768.0, 32767.0) as i16;
        self.buffer.push(output);
        output
    }
}
