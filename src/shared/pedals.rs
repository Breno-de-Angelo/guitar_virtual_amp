use strum_macros::EnumIter;

#[derive(Copy, Clone, EnumIter)]
pub enum PedalDescription {
    Amp(AmpParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    LowPass(LowPassParams),
    Flanger(FlangerParams),
}

impl PedalDescription {
    pub fn name(&self) -> &'static str {
        match self {
            PedalDescription::Amp(_) => "Amp",
            PedalDescription::Delay(_) => "Delay",
            PedalDescription::Reverb(_) => "Reverb",
            PedalDescription::LowPass(_) => "Low Pass",
            PedalDescription::Flanger(_) => "Flanger",
        }
    }
}

#[derive(Copy, Clone)]
pub struct ReverbParams {
    pub room_size: f32, // Feedback / duração do reverb (0.0 a 0.99)
    pub mix: f32,       // 0.0 = só dry, 1.0 = só wet
    pub damping: f32,   // Decaimento das altas frequências (0.0 a 0.99)
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self {
            room_size: 0.5,
            mix: 0.3,
            damping: 0.3,
        }
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
    pub delay_ms: f32, // tempo do delay principal em ms
    pub feedback: f32, // quanto do sinal volta ao buffer (0.0 a 0.99)
    pub gain: f32,     // volume do delay na saída (0.0 a 1.0)
    pub mix: f32,      // wet/dry mix (0.0 dry, 1.0 wet)
    pub taps: usize,   // número de ecos (taps)
    pub damping: f32,  // filtro passa-baixa no feedback (0.0 a 1.0)
}

impl Default for DelayParams {
    fn default() -> Self {
        Self {
            delay_ms: 500.0,
            feedback: 0.5,
            gain: 0.8,
            mix: 0.5,
            taps: 4,
            damping: 0.3,
        }
    }
}

#[derive(Copy, Clone)]
pub struct FlangerParams {
    pub delay_range: f32,
    pub delay_rate: f32,
    pub gain: f32,
}

impl Default for FlangerParams {
    fn default() -> Self {
        Self {
            delay_range: 2.0,
            delay_rate: 0.5,
            gain: 0.5,
        }
    }
}
