use crate::{
    backend::pedals::{
        amp::Amp, chorus::Chorus, compressor::Compressor, delay::Delay, distortion::Distortion,
        eq::EqPedal, flanger::Flanger, low_pass::LowPass, noise_gate::NoiseGate, phaser::Phaser,
        reverb::Reverb, tremolo::Tremolo, wah_wah::WahWah,
    },
    shared::pedals::PedalDescription,
};

pub trait Pedal: Send {
    fn apply_effect(&mut self, input: i16) -> i16;
}

pub struct PedalChain {
    pedals: Vec<Box<dyn Pedal>>,
}

impl Default for PedalChain {
    fn default() -> Self {
        Self::new()
    }
}

impl PedalChain {
    pub fn new() -> Self {
        PedalChain { pedals: Vec::new() }
    }

    pub fn from_description(description: Vec<PedalDescription>) -> Self {
        PedalChain {
            pedals: description
                .iter()
                .map(|&pedal_description| match pedal_description {
                    PedalDescription::Reverb(params) => {
                        Box::new(Reverb::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Amp(params) => Box::new(Amp::new(params)) as Box<dyn Pedal>,
                    PedalDescription::LowPass(params) => {
                        Box::new(LowPass::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Delay(params) => {
                        Box::new(Delay::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Flanger(params) => {
                        Box::new(Flanger::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::WahWah(params) => {
                        Box::new(WahWah::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Distortion(params) => {
                        Box::new(Distortion::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::NoiseGate(params) => {
                        Box::new(NoiseGate::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Compressor(params) => {
                        Box::new(Compressor::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Tremolo(params) => {
                        Box::new(Tremolo::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Chorus(params) => {
                        Box::new(Chorus::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Eq(params) => {
                        Box::new(EqPedal::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Phaser(params) => {
                        Box::new(Phaser::new(params)) as Box<dyn Pedal>
                    }
                })
                .collect(),
        }
    }

    pub fn process_sample(&mut self, mut sample: i16) -> i16 {
        for pedal in &mut self.pedals {
            sample = pedal.apply_effect(sample);
        }
        sample
    }
}

/// Routes N independent input channels through N independent mono `PedalChain`
/// instances (one per channel), all built from the same `PedalDescription`s via
/// `PedalChain::from_description`. This is the "least invasive" way to support
/// multi-channel devices without making the `Pedal` trait itself stereo/N-channel
/// aware: each channel simply gets its own fresh copy of the whole chain, so pedal
/// state (delay lines, filter memory, etc.) never leaks between channels.
///
/// With `channel_count == 1` this behaves exactly like a single `PedalChain`
/// (the default/legacy mono path), so existing single-channel behavior is unchanged.
pub struct MultiChannelPedalChain {
    channels: Vec<PedalChain>,
}

impl MultiChannelPedalChain {
    /// Builds `channel_count` independent chains, each freshly constructed from a
    /// clone of `description`. `channel_count` is clamped to at least 1.
    pub fn from_description(description: Vec<PedalDescription>, channel_count: usize) -> Self {
        let channel_count = channel_count.max(1);
        MultiChannelPedalChain {
            channels: (0..channel_count)
                .map(|_| PedalChain::from_description(description.clone()))
                .collect(),
        }
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Processes one sample through the chain owned by `channel`. `channel` is
    /// wrapped (via modulo) into range so a caller can't panic by passing an
    /// out-of-range index.
    pub fn process_sample(&mut self, channel: usize, sample: i16) -> i16 {
        let idx = channel % self.channels.len();
        self.channels[idx].process_sample(sample)
    }

    /// Processes a full frame (one sample per channel, in channel order) and
    /// returns the processed frame. If `frame` has fewer entries than
    /// `channel_count`, only the first `frame.len()` channels are processed; if it
    /// has more, extra entries are processed via channel index modulo (see
    /// `process_sample`).
    pub fn process_frame(&mut self, frame: &[i16]) -> Vec<i16> {
        frame
            .iter()
            .enumerate()
            .map(|(channel, &sample)| self.process_sample(channel, sample))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::pedals::LowPassParams;

    /// Proves that two channels run through `MultiChannelPedalChain` are fully
    /// independent: a `LowPass` pedal has internal state (`last_sample`) that
    /// would leak across channels if they shared a single `PedalChain` instance.
    /// Channel 0 gets a loud alternating signal (known sequence), channel 1 gets
    /// pure silence. If state leaked, channel 1's filtered output would be nudged
    /// away from zero by channel 0's samples.
    #[test]
    fn multi_channel_pedal_chain_keeps_channels_independent() {
        let description = vec![PedalDescription::LowPass(LowPassParams { frequency: 1000.0 })];
        let mut chain = MultiChannelPedalChain::from_description(description, 2);
        assert_eq!(chain.channel_count(), 2);

        let channel0_input: [i16; 5] = [20000, -20000, 20000, -20000, 20000];
        let channel1_input: [i16; 5] = [0, 0, 0, 0, 0];

        let mut channel0_outputs = Vec::new();
        let mut channel1_outputs = Vec::new();

        for i in 0..channel0_input.len() {
            let frame = [channel0_input[i], channel1_input[i]];
            let processed = chain.process_frame(&frame);
            channel0_outputs.push(processed[0]);
            channel1_outputs.push(processed[1]);
        }

        // Channel 0 (fed a real signal) should show the low-pass filter reacting:
        // its output should not simply be all zeros.
        assert!(channel0_outputs.iter().any(|&s| s != 0));

        // Channel 1 (fed pure silence) must stay exactly silent throughout -- if
        // channel 0's state leaked into channel 1's chain this would fail.
        assert!(channel1_outputs.iter().all(|&s| s == 0));
    }

    /// Proves that with a single channel, `MultiChannelPedalChain` behaves
    /// identically to a plain `PedalChain` -- i.e. the default/legacy mono path
    /// is unaffected by the new multi-channel machinery.
    #[test]
    fn multi_channel_pedal_chain_with_one_channel_matches_plain_chain() {
        let description = vec![PedalDescription::LowPass(LowPassParams { frequency: 1000.0 })];
        let mut multi = MultiChannelPedalChain::from_description(description.clone(), 1);
        let mut plain = PedalChain::from_description(description);

        let input: [i16; 6] = [1000, -1000, 5000, -5000, 0, 32000];
        for &sample in &input {
            let multi_out = multi.process_sample(0, sample);
            let plain_out = plain.process_sample(sample);
            assert_eq!(multi_out, plain_out);
        }
    }
}
