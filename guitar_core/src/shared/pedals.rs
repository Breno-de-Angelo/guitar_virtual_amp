use strum_macros::EnumIter;

#[derive(Copy, Clone, EnumIter)]
pub enum PedalDescription {
    Amp(AmpParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    LowPass(LowPassParams),
    Flanger(FlangerParams),
    WahWah(WahWahParams),
    Distortion(DistortionParams),
    Chorus(ChorusParams),
}

impl PedalDescription {
    pub fn name(&self) -> &'static str {
        match self {
            PedalDescription::Amp(_) => "Amp",
            PedalDescription::Delay(_) => "Delay",
            PedalDescription::Reverb(_) => "Reverb",
            PedalDescription::LowPass(_) => "Low Pass",
            PedalDescription::Flanger(_) => "Flanger",
            PedalDescription::WahWah(_) => "WahWah",
            PedalDescription::Distortion(_) => "Distortion",
            PedalDescription::Chorus(_) => "Chorus",
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

#[derive(Copy, Clone)]
pub struct WahWahParams {
    pub frequency: f32,    // Center frequency of the wah filter (Hz)
    pub resonance: f32,    // Q factor / resonance (0.1 to 10.0)
    pub mix: f32,          // Wet/dry mix (0.0 dry, 1.0 wet)
    pub lfo_rate: f32,     // LFO rate for auto-wah (Hz)
    pub lfo_depth: f32,    // LFO depth (0.0 to 1.0)
}

impl Default for WahWahParams {
    fn default() -> Self {
        Self {
            frequency: 1000.0,
            resonance: 2.0,
            mix: 0.5,
            lfo_rate: 2.0,
            lfo_depth: 0.5,
        }
    }
}

#[derive(Copy, Clone)]
pub struct DistortionParams {
    pub drive: f32,        // Amount of distortion/saturation (0.0 to 10.0)
    pub tone: f32,         // Tone control - high frequency rolloff (0.0 to 1.0)
    pub level: f32,        // Output level (0.0 to 2.0)
    pub mix: f32,          // Wet/dry mix (0.0 dry, 1.0 wet)
    pub bias: f32,         // DC bias offset (-1.0 to 1.0)
}

impl Default for DistortionParams {
    fn default() -> Self {
        Self {
            drive: 3.0,
            tone: 0.5,
            level: 0.8,
            mix: 0.7,
            bias: 0.0,
        }
    }
}

#[derive(Copy, Clone)]
pub struct ChorusParams {
    pub delay_ms: f32, // base delay time (ms)
    pub depth_ms: f32, // how much the LFO modulates the delay time (ms)
    pub rate_hz: f32,  // LFO rate (Hz)
    pub mix: f32,      // wet/dry mix (0.0 dry, 1.0 wet)
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self {
            delay_ms: 20.0,
            depth_ms: 5.0,
            rate_hz: 1.0,
            mix: 0.5,
        }
    }
}
