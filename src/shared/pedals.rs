#[derive(Copy, Clone)]
pub enum PedalDescription {
    Amp(AmpParams),
    Reverb(ReverbParams),
    LowPass(LowPassParams),
}

#[derive(Copy, Clone)]
pub struct ReverbParams {
    pub feedback: f32,
}

impl ReverbParams {
    pub fn new(feedback: f32) -> Self {
        Self { feedback }
    }
}

#[derive(Copy, Clone)]
pub struct AmpParams {
    pub gain: f32,
}

impl AmpParams {
    pub fn new(gain: f32) -> Self {
        Self { gain }
    }
}

#[derive(Copy, Clone)]
pub struct LowPassParams {
    pub frequency: f32,
}

impl LowPassParams {
    pub fn new(frequency: f32) -> Self {
        Self { frequency }
    }
}
