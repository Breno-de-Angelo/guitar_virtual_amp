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
    NoiseGate(NoiseGateParams),
    Compressor(CompressorParams),
    Tremolo(TremoloParams),
    Chorus(ChorusParams),
    Eq(EqParams),
    Phaser(PhaserParams),
    Octaver(OctaverParams),
    PitchShift(PitchShifterParams),
    Looper(LooperParams),
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
            PedalDescription::NoiseGate(_) => "Noise Gate",
            PedalDescription::Compressor(_) => "Compressor",
            PedalDescription::Tremolo(_) => "Tremolo",
            PedalDescription::Chorus(_) => "Chorus",
            PedalDescription::Eq(_) => "EQ",
            PedalDescription::Phaser(_) => "Phaser",
            PedalDescription::Octaver(_) => "Octaver",
            PedalDescription::PitchShift(_) => "Pitch Shifter",
            PedalDescription::Looper(_) => "Looper",
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

/// Transport command for the Looper pedal. Sent from the UI (buttons, not a
/// slider) through `LooperParams::command` so it round-trips over the existing
/// `PedalDescription` update channel like any other parameter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LooperCommand {
    /// Pass audio through unchanged; no recording/playback happening.
    #[default]
    Idle,
    /// Record incoming audio into the loop buffer while passing it through dry.
    Record,
    /// Play the recorded buffer back on a loop.
    Play,
    /// Play the recorded buffer back on a loop while mixing in and writing back
    /// (layering) the live input at the current playback position.
    Overdub,
    /// Stop transport (see `Looper::apply_effect` doc comment for exact
    /// stopped-state audio behavior).
    Stop,
    /// Empty the recorded buffer and reset playback position.
    Clear,
}

/// NOTE: because `PedalChain` is rebuilt wholesale from `Vec<PedalDescription>`
/// on every chain edit (see CLAUDE.md's "Pedal chain" architecture notes), this
/// looper's recorded buffer is lost whenever ANY pedal in the chain changes, not
/// just this one -- moving the transport command (Record/Play/Overdub/Stop/Clear)
/// live is a limitation of the current single-shot chain-rebuild architecture.
/// This is tracked as a known limitation for a future architectural revisit
/// (e.g. giving pedals a way to receive in-place parameter updates instead of a
/// full rebuild), not something to silently work around here.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LooperParams {
    /// Capacity of the loop buffer, in seconds. Determines how long a loop can be
    /// before older audio would need to be discarded/truncated.
    pub max_loop_seconds: f32,
    /// Current transport command, set by the UI's transport buttons.
    pub command: LooperCommand,
}

impl Default for LooperParams {
    fn default() -> Self {
        Self {
            max_loop_seconds: 30.0,
            command: LooperCommand::Idle,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PitchShifterParams {
    pub semitones: f32,     // Pitch shift amount in semitones (-12.0 to +12.0), 0.0 = no shift
    pub mix: f32,           // Wet/dry mix (0.0 dry, 1.0 wet)
    pub grain_size_ms: f32, // Overlap-add grain/window size in ms (~50-100ms typical)
}

impl Default for PitchShifterParams {
    fn default() -> Self {
        Self {
            semitones: 0.0,
            mix: 0.5,
            grain_size_ms: 80.0,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OctaverParams {
    pub octave_down_mix: f32, // how much of the octave-down (rectified) signal to mix in (0.0 to 1.0)
    pub dry_mix: f32,        // how much of the original dry signal to keep (0.0 to 1.0)
    pub tone: f32,           // low-pass tone control on the octave-down signal (0.0 to 1.0)
}

impl Default for OctaverParams {
    fn default() -> Self {
        Self {
            octave_down_mix: 0.5,
            dry_mix: 0.7,
            tone: 0.5,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqParams {
    pub low_gain_db: f32,  // Bass shelf gain (~100Hz), -12.0 to +12.0 dB
    pub mid_gain_db: f32,  // Mid peak gain (~1000Hz), -12.0 to +12.0 dB
    pub high_gain_db: f32, // Treble shelf gain (~5000Hz), -12.0 to +12.0 dB
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            low_gain_db: 0.0,
            mid_gain_db: 0.0,
            high_gain_db: 0.0,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhaserParams {
    pub rate_hz: f32,  // LFO sweep rate (Hz)
    pub depth: f32,    // How much the LFO sweeps the all-pass center frequency (0.0 to 1.0)
    pub feedback: f32, // Resonance feeding filtered signal back for a stronger effect (0.0 to 0.95)
    pub mix: f32,      // Wet/dry mix (0.0 dry, 1.0 wet)
}

impl Default for PhaserParams {
    fn default() -> Self {
        Self {
            rate_hz: 0.5,
            depth: 0.7,
            feedback: 0.3,
            mix: 0.5,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TremoloParams {
    pub rate_hz: f32, // LFO frequency (0.5 to 10.0 Hz)
    pub depth: f32,   // 0.0 = no effect, 1.0 = full modulation down to silence
}

impl Default for TremoloParams {
    fn default() -> Self {
        Self {
            rate_hz: 5.0,
            depth: 0.5,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompressorParams {
    pub threshold: f32,   // Linear amplitude above which gain reduction kicks in (0.0 to 1.0)
    pub ratio: f32,       // Compression ratio (1.0 = no compression, 4.0 = 4:1, etc.)
    pub attack_ms: f32,   // Envelope attack time in ms (how fast gain reduction engages)
    pub release_ms: f32,  // Envelope release time in ms (how fast gain reduction recovers)
    pub makeup_gain: f32, // Linear gain applied after compression to restore overall level
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 100.0,
            makeup_gain: 1.5,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseGateParams {
    pub threshold: f32, // Linear amplitude below which the gate closes (0.0 to 1.0)
    pub attack_ms: f32, // How fast the gate opens once the signal rises above threshold
    pub release_ms: f32, // How fast the gate closes once the signal drops below threshold (after hold)
    pub hold_ms: f32, // How long to stay open after dropping below threshold before releasing
}

impl Default for NoiseGateParams {
    fn default() -> Self {
        Self {
            threshold: 0.02,
            attack_ms: 1.0,
            release_ms: 100.0,
            hold_ms: 50.0,
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
