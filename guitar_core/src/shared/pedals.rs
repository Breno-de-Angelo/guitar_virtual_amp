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
            PedalDescription::Looper(_) => "Looper",
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

/// Transport command for the Looper pedal. Sent from the UI (buttons, not a
/// slider) through `LooperParams::command` so it round-trips over the existing
/// `PedalDescription` update channel like any other parameter.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
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
#[derive(Copy, Clone)]
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
