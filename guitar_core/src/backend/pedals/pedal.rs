use crate::{
    backend::pedals::{
        amp::Amp, cabinet::Cabinet, chorus::Chorus, compressor::Compressor, delay::Delay,
        distortion::Distortion, eq::EqPedal, flanger::Flanger, looper::Looper, low_pass::LowPass,
        noise_gate::NoiseGate, octaver::Octaver, phaser::Phaser, pitch_shifter::PitchShifter,
        reverb::Reverb, tremolo::Tremolo, wah_wah::WahWah,
    },
    shared::pedals::{PedalDescription, PedalInstance},
};

pub trait Pedal: Send {
    fn apply_effect(&mut self, input: i16) -> i16;

    /// Attempts to apply `description`'s params to this pedal in place,
    /// preserving internal DSP state (delay lines, loop buffers, LFO phase,
    /// etc.) instead of the caller reconstructing the pedal from scratch.
    /// Returns `true` if applied, `false` to tell the caller to fall back to
    /// full reconstruction (the default, and today's behavior for every
    /// pedal except `Looper`).
    ///
    /// This exists because `PedalChain` used to be rebuilt wholesale from
    /// `Vec<PedalInstance>` on *every* chain edit -- including edits to a
    /// pedal's own transport/parameters, not just other pedals in the chain.
    /// For most pedals a fresh instance is harmless (a knob tweak
    /// legitimately restarting an LFO phase or clearing a delay tail isn't
    /// noticeable), but for `Looper` it was a correctness bug: clicking
    /// "Play" right after "Record" sent an updated chain description, which
    /// rebuilt the `Looper` from scratch and silently threw away the just-
    /// recorded buffer before a single loop iteration ever played back.
    fn update_in_place(&mut self, _description: &PedalDescription) -> bool {
        false
    }
}

pub struct PedalChain {
    pedals: Vec<PedalEntry>,
}

/// One live pedal in the chain: whether it's bypassed, the discriminant of
/// the `PedalDescription` variant it was built from (used by
/// `apply_description` to detect a pedal-type change without needing
/// downcasting/`Any`), and the pedal instance itself.
struct PedalEntry {
    enabled: bool,
    kind: std::mem::Discriminant<PedalDescription>,
    pedal: Box<dyn Pedal>,
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

    pub fn from_description(description: Vec<PedalInstance>) -> Self {
        PedalChain {
            pedals: description
                .into_iter()
                .map(|instance| PedalEntry {
                    enabled: instance.enabled,
                    kind: std::mem::discriminant(&instance.description),
                    pedal: Self::build_pedal(instance.description),
                })
                .collect(),
        }
    }

    /// Applies an updated chain description to an already-running chain,
    /// preferring in-place updates (see `Pedal::update_in_place`) over
    /// reconstruction so pedals with meaningful session state (the `Looper`)
    /// survive edits to *other* pedals in the chain, or to their own
    /// transport/parameters. Falls back to a full rebuild (`from_description`)
    /// if the chain's length changed (a pedal was added/removed/reordered),
    /// since positions no longer line up 1:1 with the previous chain.
    pub fn apply_description(&mut self, description: Vec<PedalInstance>) {
        if description.len() != self.pedals.len() {
            *self = Self::from_description(description);
            return;
        }

        for (entry, instance) in self.pedals.iter_mut().zip(description.into_iter()) {
            entry.enabled = instance.enabled;
            let new_kind = std::mem::discriminant(&instance.description);
            let updated_in_place =
                new_kind == entry.kind && entry.pedal.update_in_place(&instance.description);
            if !updated_in_place {
                entry.kind = new_kind;
                entry.pedal = Self::build_pedal(instance.description);
            }
        }
    }

    fn build_pedal(pedal_description: PedalDescription) -> Box<dyn Pedal> {
        match pedal_description {
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
                    PedalDescription::Octaver(params) => {
                        Box::new(Octaver::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::PitchShift(params) => {
                        Box::new(PitchShifter::new(params)) as Box<dyn Pedal>
                    }
                    PedalDescription::Looper(params) => {
                        Box::new(Looper::new(params)) as Box<dyn Pedal>
                    }
            PedalDescription::Cabinet(params) => {
                Box::new(Cabinet::new(params)) as Box<dyn Pedal>
            }
        }
    }

    pub fn process_sample(&mut self, mut sample: i16) -> i16 {
        for entry in &mut self.pedals {
            if entry.enabled {
                sample = entry.pedal.apply_effect(sample);
            }
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
    pub fn from_description(description: Vec<PedalInstance>, channel_count: usize) -> Self {
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
    use crate::shared::pedals::{AmpParams, LooperCommand, LooperParams, LowPassParams, PedalInstance};

    /// A bypassed pedal must leave the sample untouched even though it's still
    /// instantiated in the chain, proving "soft bypass" (3.1) doesn't just
    /// filter the pedal out at build time.
    #[test]
    fn disabled_pedal_is_skipped_but_stays_in_chain() {
        let mut instance = PedalInstance::new(PedalDescription::Amp(AmpParams {
            gain: 5.0,
            ..Default::default()
        }));
        instance.enabled = false;
        let mut chain = PedalChain::from_description(vec![instance]);

        // Amp with gain=5.0 would clip/change a nonzero sample; bypassed, it
        // must come out exactly as it went in.
        assert_eq!(chain.process_sample(1000), 1000);
    }

    /// Regression test for the reported "loop pedal isn't working" bug: the UI
    /// sends the *entire* updated chain description down `pedal_tx` on every
    /// edit, including the Looper's own transport button clicks. Before
    /// `Pedal::update_in_place`, `apply_description` (like the old
    /// unconditional `from_description`) would reconstruct a fresh, empty
    /// `Looper` the moment the command changed from Record to Play, so the
    /// just-recorded loop never played back. Simulates exactly that sequence.
    #[test]
    fn looper_survives_transport_change_via_apply_description() {
        let mut chain = PedalChain::from_description(vec![PedalInstance::new(
            PedalDescription::Looper(LooperParams {
                max_loop_seconds: 1.0,
                command: LooperCommand::Record,
            }),
        )]);

        let recorded = [100_i16, 200, 300, 400];
        for &sample in &recorded {
            chain.process_sample(sample);
        }

        // This mirrors the UI clicking "Play": a whole new chain description
        // is sent through the same channel `apply_description` consumes.
        chain.apply_description(vec![PedalInstance::new(PedalDescription::Looper(
            LooperParams {
                max_loop_seconds: 1.0,
                command: LooperCommand::Play,
            },
        ))]);

        let played_back: Vec<i16> = recorded.iter().map(|_| chain.process_sample(0)).collect();
        assert_eq!(played_back, recorded);
    }

    /// A pedal elsewhere in the chain being tweaked (a different `PedalInstance`
    /// in the same `Vec` sent down `pedal_tx`) must not reset the Looper's
    /// buffer either -- the bug wasn't specific to the Looper's own transport
    /// changes, any chain-description update triggered the same reconstruction.
    #[test]
    fn looper_survives_unrelated_pedal_update_via_apply_description() {
        let mut chain = PedalChain::from_description(vec![
            PedalInstance::new(PedalDescription::Looper(LooperParams {
                max_loop_seconds: 1.0,
                command: LooperCommand::Record,
            })),
            PedalInstance::new(PedalDescription::Amp(AmpParams::default())),
        ]);

        let recorded = [111_i16, 222, 333];
        for &sample in &recorded {
            chain.process_sample(sample);
        }

        // Tweak the Amp's gain and switch the Looper to Play in the same update,
        // exactly like the UI sending the whole chain after any single edit.
        chain.apply_description(vec![
            PedalInstance::new(PedalDescription::Looper(LooperParams {
                max_loop_seconds: 1.0,
                command: LooperCommand::Play,
            })),
            PedalInstance::new(PedalDescription::Amp(AmpParams {
                gain: 2.0,
                ..Default::default()
            })),
        ]);

        // Amp is unity-ish at low gain so the recorded loop should come back
        // essentially unchanged in sign/order; the key assertion is that it
        // isn't silence (i.e. the buffer wasn't wiped).
        let played_back: Vec<i16> = recorded.iter().map(|_| chain.process_sample(0)).collect();
        assert!(played_back.iter().any(|&s| s != 0));
    }

    /// Proves that two channels run through `MultiChannelPedalChain` are fully
    /// independent: a `LowPass` pedal has internal state (`last_sample`) that
    /// would leak across channels if they shared a single `PedalChain` instance.
    /// Channel 0 gets a loud alternating signal (known sequence), channel 1 gets
    /// pure silence. If state leaked, channel 1's filtered output would be nudged
    /// away from zero by channel 0's samples.
    #[test]
    fn multi_channel_pedal_chain_keeps_channels_independent() {
        let description = vec![PedalInstance::new(PedalDescription::LowPass(LowPassParams { frequency: 1000.0 }))];
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
        let description = vec![PedalInstance::new(PedalDescription::LowPass(LowPassParams { frequency: 1000.0 }))];
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
