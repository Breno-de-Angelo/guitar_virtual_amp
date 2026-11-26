//! Bass and drums under the guitar, played by rustysynth (the tab player's
//! synth) from a General MIDI SoundFont, continuous over the whole video.

use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::guitar::{RATE, Rng};

#[derive(Clone, Copy, Debug)]
pub enum Msg {
    Program { channel: i32, program: i32 },
    On { channel: i32, key: i32, velocity: i32 },
    Off { channel: i32, key: i32 },
    Volume { channel: i32, value: i32 },
}

pub const BASS_CHANNEL: i32 = 0;
pub const DRUM_CHANNEL: i32 = 9;

pub fn load_soundfont(path: &std::path::Path) -> Result<Arc<SoundFont>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    SoundFont::new(&mut file).map(Arc::new).map_err(|e| format!("{}: {e:?}", path.display()))
}

/// Events at `(seconds, message)`, rendered to stereo.
pub fn render(sf: &Arc<SoundFont>, mut events: Vec<(f64, Msg)>, len: usize) -> Result<(Vec<f32>, Vec<f32>), String> {
    let settings = SynthesizerSettings::new(RATE as i32);
    let mut synth = Synthesizer::new(sf, &settings).map_err(|e| format!("synth: {e:?}"))?;
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut left, mut right) = (vec![0f32; len], vec![0f32; len]);
    let mut at = 0usize;
    for (t, msg) in events {
        let sample = ((t.max(0.0) * RATE as f64) as usize).min(len);
        if sample > at {
            synth.render(&mut left[at..sample], &mut right[at..sample]);
            at = sample;
        }
        match msg {
            Msg::Program { channel, program } => synth.process_midi_message(channel, 0xC0, program, 0),
            Msg::On { channel, key, velocity } => synth.note_on(channel, key, velocity),
            Msg::Off { channel, key } => synth.note_off(channel, key),
            Msg::Volume { channel, value } => synth.process_midi_message(channel, 0xB0, 7, value),
        }
    }
    if at < len {
        synth.render(&mut left[at..], &mut right[at..]);
    }
    Ok((left, right))
}

/// Small timing and velocity differences, like a drummer and a bassist
/// rather than a sequencer.
pub fn humanize(events: &mut [(f64, Msg)], seed: u64) {
    let mut rng = Rng::new(seed);
    for (t, msg) in events.iter_mut() {
        if let Msg::On { velocity, .. } = msg {
            *t += rng.next() * 0.004;
            *velocity = (*velocity as f64 * (1.0 + rng.next() * 0.07)).clamp(1.0, 127.0) as i32;
        }
    }
}
