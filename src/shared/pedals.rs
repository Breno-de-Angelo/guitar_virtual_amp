#[derive(Copy, Clone)]
pub enum PedalDescription {
    Amp(AmpParams),
    Reverb(ReverbParams),
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
