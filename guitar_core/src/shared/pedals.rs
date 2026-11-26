use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

use crate::shared::i18n::tr;

#[derive(Clone, Debug, PartialEq, EnumIter, Serialize, Deserialize)]
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
    Cabinet(CabinetParams),
    Nam(NamParams),
}

/// One entry in a pedal chain: a [`PedalDescription`] plus whether it's
/// currently bypassed. Bypassing keeps the pedal in the chain (and, once
/// rebuilt, its instance in memory) without removing it -- matching the real
/// pedalboard workflow of stomping a footswitch instead of unplugging a pedal.
///
/// Serialized with `#[serde(flatten)]` on `description` so existing preset
/// JSON files (which serialize a bare `PedalDescription`, e.g. `{"Amp": {...}}`)
/// still deserialize: `enabled` defaults to `true` when absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PedalInstance {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(flatten)]
    pub description: PedalDescription,
}

fn default_enabled() -> bool {
    true
}

impl PedalInstance {
    pub fn new(description: PedalDescription) -> Self {
        Self {
            enabled: true,
            description,
        }
    }
}

impl From<PedalDescription> for PedalInstance {
    fn from(description: PedalDescription) -> Self {
        Self::new(description)
    }
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
            PedalDescription::Cabinet(_) => "Cabinet",
            PedalDescription::Nam(_) => "NAM Model",
        }
    }

    /// What kind of effect this is, for grouping the "add pedal" menu and
    /// coloring pedal cards.
    pub fn category(&self) -> PedalCategory {
        match self {
            PedalDescription::Compressor(_) | PedalDescription::NoiseGate(_) => PedalCategory::Dynamics,
            PedalDescription::Distortion(_) => PedalCategory::Drive,
            PedalDescription::Amp(_) | PedalDescription::Nam(_) | PedalDescription::Cabinet(_) => {
                PedalCategory::Amp
            }
            PedalDescription::Eq(_) | PedalDescription::LowPass(_) | PedalDescription::WahWah(_) => {
                PedalCategory::Filter
            }
            PedalDescription::Chorus(_)
            | PedalDescription::Flanger(_)
            | PedalDescription::Phaser(_)
            | PedalDescription::Tremolo(_) => PedalCategory::Modulation,
            PedalDescription::Octaver(_) | PedalDescription::PitchShift(_) => PedalCategory::Pitch,
            PedalDescription::Delay(_) | PedalDescription::Reverb(_) => PedalCategory::Time,
            PedalDescription::Looper(_) => PedalCategory::Utility,
        }
    }

    /// One line describing the effect, shown in the "add pedal" menu.
    pub fn blurb(&self) -> &'static str {
        match self {
            PedalDescription::Amp(_) => tr(
                "Preamp with Clean/Crunch/Lead voicings and a tone stack",
                "Pré-amp com voicings Clean/Crunch/Lead e equalização",
            ),
            PedalDescription::Delay(_) => tr(
                "Echoes with feedback, several taps and damping",
                "Ecos com feedback, várias repetições e amortecimento",
            ),
            PedalDescription::Reverb(_) => tr("Room ambience", "Ambiência de sala"),
            PedalDescription::LowPass(_) => tr("Cuts the highs above a frequency", "Corta os agudos acima de uma frequência"),
            PedalDescription::Flanger(_) => tr(
                "Jet-like sweep from a short modulated delay",
                "Efeito de jato de um delay curto modulado",
            ),
            PedalDescription::WahWah(_) => tr(
                "Resonant filter swept by an LFO (auto-wah)",
                "Filtro ressonante varrido por um LFO (auto-wah)",
            ),
            PedalDescription::Distortion(_) => tr(
                "Overdrive / distortion with tone and bias",
                "Overdrive / distorção com tone e bias",
            ),
            PedalDescription::NoiseGate(_) => tr("Silences hum and hiss between notes", "Silencia ruído e chiado entre as notas"),
            PedalDescription::Compressor(_) => tr("Evens out the dynamics, adds sustain", "Equilibra a dinâmica e dá sustain"),
            PedalDescription::Tremolo(_) => tr("Pulsing volume", "Volume pulsante"),
            PedalDescription::Chorus(_) => tr(
                "Thickens the sound with a detuned copy",
                "Encorpa o som com uma cópia desafinada",
            ),
            PedalDescription::Eq(_) => tr("Three-band equalizer", "Equalizador de três bandas"),
            PedalDescription::Phaser(_) => tr(
                "Swirling notches from all-pass filters",
                "Ondulação de filtros passa-tudo",
            ),
            PedalDescription::Octaver(_) => tr("Adds the octave below", "Adiciona a oitava abaixo"),
            PedalDescription::PitchShift(_) => tr(
                "Shifts the pitch up or down by semitones",
                "Sobe ou desce o tom em semitons",
            ),
            PedalDescription::Looper(_) => tr(
                "Records a loop and plays along with it",
                "Grava um loop para você tocar junto",
            ),
            PedalDescription::Cabinet(_) => tr(
                "Speaker cabinet from an impulse response (.wav)",
                "Gabinete de alto-falante a partir de uma resposta ao impulso (.wav)",
            ),
            PedalDescription::Nam(_) => tr(
                "Neural Amp Modeler capture (.nam) of a real amp or pedal",
                "Captura do Neural Amp Modeler (.nam) de um amp ou pedal real",
            ),
        }
    }
}

/// Effect families, in signal-chain order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum_macros::EnumIter)]
pub enum PedalCategory {
    Dynamics,
    Filter,
    Drive,
    Amp,
    Modulation,
    Pitch,
    Time,
    Utility,
}

impl PedalCategory {
    pub fn label(self) -> &'static str {
        match self {
            PedalCategory::Dynamics => tr("Dynamics", "Dinâmica"),
            PedalCategory::Filter => tr("Filter & EQ", "Filtro e EQ"),
            PedalCategory::Drive => "Drive",
            PedalCategory::Amp => tr("Amp & Cabinet", "Amp e gabinete"),
            PedalCategory::Modulation => tr("Modulation", "Modulação"),
            PedalCategory::Pitch => "Pitch",
            PedalCategory::Time => tr("Delay & Reverb", "Delay e reverb"),
            PedalCategory::Utility => "Looper",
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmpVoicing {
    Clean,
    Crunch,
    Lead,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmpParams {
    pub gain: f32,
    #[serde(default = "default_amp_voicing")]
    pub voicing: AmpVoicing,
    #[serde(default = "default_amp_bass")]
    pub bass: f32,
    #[serde(default = "default_amp_mid")]
    pub mid: f32,
    #[serde(default = "default_amp_treble")]
    pub treble: f32,
}

fn default_amp_voicing() -> AmpVoicing {
    AmpVoicing::Clean
}

fn default_amp_bass() -> f32 {
    0.0
}

fn default_amp_mid() -> f32 {
    0.0
}

fn default_amp_treble() -> f32 {
    0.0
}

impl Default for AmpParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            voicing: default_amp_voicing(),
            bass: default_amp_bass(),
            mid: default_amp_mid(),
            treble: default_amp_treble(),
        }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CabinetParams {
    /// Path to a `.wav` impulse response file, loaded (and re-loaded on every
    /// chain rebuild) by the `Cabinet` pedal's constructor. `None` means no IR
    /// is loaded, in which case the pedal passes audio through unchanged.
    pub ir_path: Option<String>,
    /// Wet/dry mix (0.0 dry/bypassed tone, 1.0 fully cabinet-voiced).
    pub mix: f32,
    /// Set when the IR was downloaded from TONE3000, so the pedal can show
    /// its creator and license. `None` for files the user loaded by hand.
    #[serde(default)]
    pub source: Option<Tone3000Source>,
}

impl Default for CabinetParams {
    fn default() -> Self {
        Self {
            ir_path: None,
            mix: 1.0,
            source: None,
        }
    }
}

/// Where a downloaded model or IR came from on TONE3000. Stored in the pedal
/// params (and therefore in presets) so attribution and license travel with
/// the file, and so a preset opened on another machine can fetch the same
/// tone again through the TONE3000 "load tone" flow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tone3000Source {
    pub tone_id: u64,
    pub model_id: u64,
    pub tone_title: String,
    pub model_name: String,
    /// Creator's display name (or username when they have none).
    pub creator: String,
    pub creator_username: String,
    /// Gear type as TONE3000 reports it, e.g. `amp`, `amp-cab`, `pedal`.
    pub gear: String,
    /// Tone format, `nam` or `ir`.
    pub format: String,
    /// License chosen by the creator, e.g. `t3k`, `cc-by`.
    pub license: String,
    pub tone_url: String,
    #[serde(default)]
    pub image_url: Option<String>,
}

/// Neural Amp Modeler capture (`.nam`): an amp, pedal or full rig modeled by
/// a neural network. See `backend::pedals::nam`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamParams {
    /// Path to the `.nam` file. `None` passes audio through unchanged.
    pub model_path: Option<String>,
    /// Gain applied before the model, in dB. Captures react to input level
    /// the way the real amp reacts to a hotter or weaker guitar signal.
    pub input_gain_db: f32,
    /// Gain applied after the model (and after normalization), in dB.
    pub output_gain_db: f32,
    /// Scale the output so every model plays back at a similar loudness,
    /// using the loudness value stored in the `.nam` file.
    pub normalize: bool,
    #[serde(default)]
    pub source: Option<Tone3000Source>,
}

impl Default for NamParams {
    fn default() -> Self {
        Self {
            model_path: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            normalize: true,
            source: None,
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
    fn every_pedal_has_a_category_and_blurb() {
        use strum::IntoEnumIterator;
        for pedal in PedalDescription::iter() {
            assert!(!pedal.blurb().is_empty(), "{}", pedal.name());
            let _ = pedal.category();
        }
        assert!(PedalCategory::iter().all(|c| !c.label().is_empty()));
    }

    #[test]
    fn pedal_chain_round_trips_through_json() {
        let chain: Vec<PedalDescription> = vec![
            PedalDescription::Amp(AmpParams {
                gain: 2.5,
                ..Default::default()
            }),
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
