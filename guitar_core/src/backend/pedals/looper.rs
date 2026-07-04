use crate::{
    backend::pedals::pedal::Pedal,
    shared::{
        config::GLOBAL_CONFIG,
        pedals::{LooperCommand, LooperParams},
    },
};

/// Internal transport state for this particular `Looper` instance. Separate
/// from `LooperCommand` (which is the value coming in from the UI every time
/// the chain is rebuilt) so that switching from e.g. `Record` to `Play` can be
/// detected and playback position reset to the start of the loop.
#[derive(Copy, Clone, PartialEq, Eq)]
enum LooperState {
    Idle,
    Recording,
    Playing,
    Overdubbing,
    Stopped,
}

/// Records a loop of audio and plays it back repeatedly, with the ability to
/// layer (overdub) more audio on top while looping.
///
/// NOTE: because `PedalChain` is rebuilt wholesale from `PedalDescription` on
/// every chain edit, a fresh `Looper` (with an empty buffer) is constructed
/// every time ANY pedal in the chain is added/removed/tweaked -- this is a
/// known limitation of the current architecture, not something fixed here
/// (see the doc comment on `LooperParams` in `shared/pedals.rs`).
pub struct Looper {
    params: LooperParams,
    state: LooperState,
    buffer: Vec<i16>,
    position: usize,
    max_samples: usize,
}

impl Looper {
    pub fn new(params: LooperParams) -> Self {
        let max_samples =
            (params.max_loop_seconds.max(0.0) * GLOBAL_CONFIG.sample_rate).round() as usize;

        Self {
            params,
            state: LooperState::Idle,
            buffer: Vec::new(),
            position: 0,
            max_samples: max_samples.max(1),
        }
    }

    fn sync_state(&mut self) {
        let target = match self.params.command {
            LooperCommand::Idle => LooperState::Idle,
            LooperCommand::Record => LooperState::Recording,
            LooperCommand::Play => LooperState::Playing,
            LooperCommand::Overdub => LooperState::Overdubbing,
            LooperCommand::Stop => LooperState::Stopped,
            LooperCommand::Clear => {
                self.buffer.clear();
                self.position = 0;
                LooperState::Idle
            }
        };

        // Entering playback (from something else) always restarts at the
        // beginning of the loop.
        if target != self.state
            && matches!(target, LooperState::Playing | LooperState::Overdubbing)
            && !matches!(
                self.state,
                LooperState::Playing | LooperState::Overdubbing
            )
        {
            self.position = 0;
        }

        self.state = target;
    }
}

impl Pedal for Looper {
    fn apply_effect(&mut self, input: i16) -> i16 {
        self.sync_state();

        match self.state {
            LooperState::Idle => input,
            LooperState::Recording => {
                if self.buffer.len() < self.max_samples {
                    self.buffer.push(input);
                }
                input
            }
            LooperState::Playing | LooperState::Overdubbing => {
                if self.buffer.is_empty() {
                    return input;
                }

                if self.position >= self.buffer.len() {
                    self.position = 0;
                }

                let looped_sample = self.buffer[self.position];

                let output = if self.state == LooperState::Overdubbing {
                    let mixed = (looped_sample as i32 + input as i32).clamp(-32768, 32767) as i16;
                    self.buffer[self.position] = mixed;
                    mixed
                } else {
                    looped_sample
                };

                self.position += 1;
                if self.position >= self.buffer.len() {
                    self.position = 0;
                }

                output
            }
            // Dry pass-through when stopped -- documented choice: the player
            // still hears their live signal, just without the recorded loop.
            LooperState::Stopped => input,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(command: LooperCommand) -> LooperParams {
        LooperParams {
            max_loop_seconds: 1.0,
            command,
        }
    }

    #[test]
    fn records_and_loops_playback_in_order() {
        let mut looper = Looper::new(params(LooperCommand::Record));

        let recorded = [100_i16, 200, 300, 400];
        for &sample in &recorded {
            looper.apply_effect(sample);
        }

        looper.params.command = LooperCommand::Play;

        // First pass through the loop should return exactly what was recorded,
        // in order.
        let first_pass: Vec<i16> = recorded.iter().map(|_| looper.apply_effect(0)).collect();
        assert_eq!(first_pass, recorded);

        // Second pass proves it wraps back to the start and repeats.
        let second_pass: Vec<i16> = recorded.iter().map(|_| looper.apply_effect(0)).collect();
        assert_eq!(second_pass, recorded);
    }

    #[test]
    fn clear_empties_the_buffer() {
        let mut looper = Looper::new(params(LooperCommand::Record));

        for sample in [10_i16, 20, 30] {
            looper.apply_effect(sample);
        }
        assert!(!looper.buffer.is_empty());

        looper.params.command = LooperCommand::Clear;
        looper.apply_effect(0);

        assert!(looper.buffer.is_empty());
        assert_eq!(looper.position, 0);

        // Playing an empty loop should just pass input through dry.
        looper.params.command = LooperCommand::Play;
        assert_eq!(looper.apply_effect(555), 555);
    }
}
