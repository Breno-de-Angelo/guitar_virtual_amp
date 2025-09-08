use std::f32::consts::{E, PI};

use crate::{
    backend::ring_buffer::RingBuffer,
    shared::{
        config::GLOBAL_CONFIG,
        pedals::{
            AmpParams, DelayParams, FlangerParams, LowPassParams, PedalDescription, ReverbParams,
        },
    },
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
                    PedalDescription::LowPass(params) => {
                        Box::new(LowPass::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Delay(params) => {
                        Box::new(Delay::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Flanger(params) => {
                        Box::new(Flanger::new(params)) as Box<dyn Pedal>
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
