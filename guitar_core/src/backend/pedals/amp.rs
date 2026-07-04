use crate::{
    backend::{dsp::biquad::Biquad, pedals::pedal::Pedal},
    shared::{
        config::GLOBAL_CONFIG,
        pedals::{AmpParams, AmpVoicing},
    },
};

const BASS_FREQ: f32 = 120.0;
const MID_FREQ: f32 = 800.0;
const MID_Q: f32 = 0.7;
const TREBLE_FREQ: f32 = 4000.0;

/// Soft/hard clipping curve applied post-gain, distinguishing the three amp
/// voicings: `Clean` barely clips, `Crunch` applies a moderate tanh soft
/// clip, and `Lead` drives the tanh curve much harder for a compressed,
/// sustain-heavy tone.
fn shape(sample: f32, voicing: AmpVoicing) -> f32 {
    match voicing {
        AmpVoicing::Clean => sample.clamp(-1.0, 1.0),
        AmpVoicing::Crunch => (sample * 2.0).tanh(),
        AmpVoicing::Lead => (sample * 6.0).tanh(),
    }
}

/// A simple amp simulation: pre-gain, voicing-dependent waveshaping, and a
/// 3-band (bass/mid/treble) tone stack built from biquad filters.
pub struct Amp {
    params: AmpParams,
    bass: Biquad,
    mid: Biquad,
    treble: Biquad,
}

impl Amp {
    pub fn new(params: AmpParams) -> Self {
        let sample_rate = GLOBAL_CONFIG.sample_rate;
        Self {
            params,
            bass: Biquad::low_shelf(BASS_FREQ, params.bass, sample_rate),
            mid: Biquad::peaking(MID_FREQ, MID_Q, params.mid, sample_rate),
            treble: Biquad::high_shelf(TREBLE_FREQ, params.treble, sample_rate),
        }
    }
}

impl Pedal for Amp {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let sample = (input as f32 / 32768.0) * self.params.gain;
        let sample = shape(sample, self.params.voicing);
        let sample = self.bass.process(sample);
        let sample = self.mid.process(sample);
        let sample = self.treble.process(sample);
        (sample * 32768.0).clamp(-32768.0, 32767.0) as i16
    }
}
