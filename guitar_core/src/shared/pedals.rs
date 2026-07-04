use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

#[derive(Copy, Clone, Debug, PartialEq, EnumIter, Serialize, Deserialize)]
pub enum PedalDescription {
    Amp(AmpParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    LowPass(LowPassParams),
    Flanger(FlangerParams),
    WahWah(WahWahParams),
    Distortion(DistortionParams),
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
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmpParams {
    pub gain: f32,
}

impl Default for AmpParams {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LowPassParams {
    pub frequency: f32,
}

impl Default for LowPassParams {
    fn default() -> Self {
        Self { frequency: 8000.0 }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_pedal_eq(a: &PedalDescription, b: &PedalDescription) {
        match (a, b) {
            (PedalDescription::Amp(a), PedalDescription::Amp(b)) => {
                assert_eq!(a.gain, b.gain);
            }
            (PedalDescription::Delay(a), PedalDescription::Delay(b)) => {
                assert_eq!(a.delay_ms, b.delay_ms);
                assert_eq!(a.feedback, b.feedback);
                assert_eq!(a.gain, b.gain);
                assert_eq!(a.mix, b.mix);
                assert_eq!(a.taps, b.taps);
                assert_eq!(a.damping, b.damping);
            }
            (PedalDescription::Reverb(a), PedalDescription::Reverb(b)) => {
                assert_eq!(a.room_size, b.room_size);
                assert_eq!(a.mix, b.mix);
                assert_eq!(a.damping, b.damping);
            }
            (PedalDescription::LowPass(a), PedalDescription::LowPass(b)) => {
                assert_eq!(a.frequency, b.frequency);
            }
            (PedalDescription::Flanger(a), PedalDescription::Flanger(b)) => {
                assert_eq!(a.delay_range, b.delay_range);
                assert_eq!(a.delay_rate, b.delay_rate);
                assert_eq!(a.gain, b.gain);
            }
            (PedalDescription::WahWah(a), PedalDescription::WahWah(b)) => {
                assert_eq!(a.frequency, b.frequency);
                assert_eq!(a.resonance, b.resonance);
                assert_eq!(a.mix, b.mix);
                assert_eq!(a.lfo_rate, b.lfo_rate);
                assert_eq!(a.lfo_depth, b.lfo_depth);
            }
            (PedalDescription::Distortion(a), PedalDescription::Distortion(b)) => {
                assert_eq!(a.drive, b.drive);
                assert_eq!(a.tone, b.tone);
                assert_eq!(a.level, b.level);
                assert_eq!(a.mix, b.mix);
                assert_eq!(a.bias, b.bias);
            }
            _ => panic!("pedal variant mismatch"),
        }
    }

    #[test]
    fn pedal_chain_round_trips_through_json() {
        let chain: Vec<PedalDescription> = vec![
            PedalDescription::Amp(AmpParams { gain: 2.5 }),
            PedalDescription::Delay(DelayParams::default()),
            PedalDescription::Distortion(DistortionParams::default()),
            PedalDescription::WahWah(WahWahParams::default()),
        ];

        let json = serde_json::to_string(&chain).expect("serialize chain");
        let round_tripped: Vec<PedalDescription> =
            serde_json::from_str(&json).expect("deserialize chain");

        assert_eq!(round_tripped.len(), chain.len());
        for (original, restored) in chain.iter().zip(round_tripped.iter()) {
            assert_pedal_eq(original, restored);
        }
    }
}
