use strum_macros::EnumIter;

#[derive(Copy, Clone, EnumIter)]
pub enum PedalDescription {
    Amp(AmpParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    LowPass(LowPassParams),
}

impl PedalDescription {
    pub fn name(&self) -> &'static str {
        match self {
            PedalDescription::Amp(_) => "Amp",
            PedalDescription::Delay(_) => "Delay",
            PedalDescription::Reverb(_) => "Reverb",
            PedalDescription::LowPass(_) => "Low Pass",
        }
    }
}

#[derive(Copy, Clone)]
pub struct ReverbParams {
    pub feedback: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self { feedback: 0.5 }
    }
}

#[derive(Copy, Clone)]
pub struct AmpParams {
    pub gain: f32,
}

impl Default for AmpParams {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

#[derive(Copy, Clone)]
pub struct LowPassParams {
    pub frequency: f32,
}

impl Default for LowPassParams {
    fn default() -> Self {
        Self { frequency: 8000.0 }
    }
}

#[derive(Copy, Clone)]
pub struct DelayParams {
    pub delay: f32,
    pub gain: f32,
}

impl Default for DelayParams {
    fn default() -> Self {
        Self {
            delay: 0.1,
            gain: 0.5,
        }
    }
}
