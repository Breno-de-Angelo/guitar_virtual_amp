use crate::{
    backend::ring_buffer::RingBuffer,
    shared::pedals::{AmpParams, PedalDescription, ReverbParams},
};

pub trait Pedal {
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
                })
                .collect(),
        }
    }

    // pub fn add_pedal(&mut self, pedal: Box<dyn Pedal>) {
    //     self.pedals.push(pedal);
    // }

    pub fn process_sample(&mut self, mut sample: i16) -> i16 {
        for pedal in &mut self.pedals {
            sample = pedal.apply_effect(sample);
        }
        sample
    }
}

const REVERB_DELTA_SAMPLE: usize = 2048;
pub struct Reverb {
    buffer: RingBuffer<65536, i16>,
    params: ReverbParams,
}

impl Reverb {
    pub fn new(params: ReverbParams) -> Self {
        Self {
            buffer: RingBuffer::new(),
            params,
        }
    }
}

impl Pedal for Reverb {
    fn apply_effect(&mut self, input: i16) -> i16 {
        self.buffer.push(input);
        let delayed = if self.buffer.len() >= REVERB_DELTA_SAMPLE {
            self.buffer[self.buffer.len() - REVERB_DELTA_SAMPLE]
        } else {
            0
        };
        let output =
            (input as f32 + delayed as f32 * self.params.feedback).clamp(-32768.0, 32767.0) as i16;
        output
    }
}

pub struct Amp {
    params: AmpParams,
}

impl Amp {
    pub fn new(params: AmpParams) -> Self {
        Self { params }
    }
}

impl Pedal for Amp {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let output = (input as f32 * self.params.gain).clamp(-32768.0, 32767.0) as i16;
        output
    }
}
