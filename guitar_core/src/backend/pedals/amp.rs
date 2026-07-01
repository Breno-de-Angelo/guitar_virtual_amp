use crate::{backend::pedals::pedal::Pedal, shared::pedals::AmpParams};

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
