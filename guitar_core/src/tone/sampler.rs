//! A dry signal for the target's notes built from real DI recordings of
//! single notes (Guitar-TECHS "allsinglenotes", CC BY 4.0: every pitch of a
//! guitar, plucked and held). Each transcribed note gets the recorded note
//! of the nearest pitch, re-pitched if needed, scaled by velocity and cut
//! at the note's end with a short fade. Compared with [`super::pluck`] it
//! has a real pickup's timbre, attack and decay, from another guitar.

use super::pluck::Note;
use crate::backend::offline::RENDER_RATE;
use std::path::Path;

/// Recorded audio kept before the labelled onset, so the attack is whole.
const PRE_ROLL_SECS: f64 = 0.01;
/// Fade at the end of a note (string damped).
const RELEASE_SECS: f64 = 0.06;

struct Sample {
    pitch: u8,
    velocity: f32,
    audio: Vec<f32>,
}

/// Recorded single notes, one per pitch, at [`RENDER_RATE`].
pub struct Sampler {
    samples: Vec<Sample>,
}

/// Parses a notes file: `onset offset pitch [velocity 0-127] ...` per line.
pub fn read_notes(text: &str) -> Vec<Note> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some(Note {
                start: f.first()?.parse().ok()?,
                end: f.get(1)?.parse().ok()?,
                pitch: f.get(2)?.parse::<f32>().ok()?.round() as u8,
                velocity: f.get(3).and_then(|v| v.parse::<f32>().ok()).unwrap_or(90.0) / 127.0,
            })
        })
        .collect()
}

impl Sampler {
    /// Cuts the labelled notes out of `(DI recording, notes file)` pairs.
    /// Later recordings only fill pitches the earlier ones lack.
    pub fn load(recordings: &[(&Path, &Path)]) -> Result<Self, String> {
        let mut samples: Vec<Sample> = Vec::new();
        for (wav, notes) in recordings {
            let audio = crate::backend::audio_file::decode_file(wav)?;
            let audio = crate::transcribe::resample::resample(&audio.samples, audio.sample_rate, RENDER_RATE);
            let text = std::fs::read_to_string(notes).map_err(|e| format!("{notes:?}: {e}"))?;
            let mut notes = read_notes(&text);
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));
            for (i, n) in notes.iter().enumerate() {
                if samples.iter().any(|s| s.pitch == n.pitch) {
                    continue;
                }
                // Up to the next note's onset: the string keeps ringing past
                // the label's offset in these recordings.
                let next = notes.get(i + 1).map_or(n.end + 1.0, |m| m.start);
                let start = ((n.start - PRE_ROLL_SECS).max(0.0) * RENDER_RATE as f64) as usize;
                let end = ((next - PRE_ROLL_SECS) * RENDER_RATE as f64) as usize;
                if end <= start + RENDER_RATE as usize / 10 || end > audio.len() {
                    continue;
                }
                samples.push(Sample {
                    pitch: n.pitch,
                    velocity: n.velocity.max(0.05),
                    audio: audio[start..end].to_vec(),
                });
            }
        }
        if samples.is_empty() {
            return Err("no single notes found".to_string());
        }
        samples.sort_by_key(|s| s.pitch);
        Ok(Self { samples })
    }

    /// Pitches covered.
    pub fn pitches(&self) -> Vec<u8> {
        self.samples.iter().map(|s| s.pitch).collect()
    }

    /// Plays `notes` into `len` samples at [`RENDER_RATE`].
    pub fn render(&self, notes: &[Note], len: usize) -> Vec<f32> {
        let rate = RENDER_RATE as f64;
        let mut out = vec![0f32; len];
        let release = (RELEASE_SECS * rate) as usize;
        for n in notes {
            let s = self
                .samples
                .iter()
                .min_by_key(|s| (s.pitch as i32 - n.pitch as i32).abs())
                .expect("at least one sample");
            // Read speed: > 1 plays higher.
            let speed = 2f64.powf((n.pitch as f64 - s.pitch as f64) / 12.0);
            let avail = ((s.audio.len() - 1) as f64 / speed) as usize;
            let held = ((n.end - n.start + PRE_ROLL_SECS) * rate) as usize;
            let length = (held + release).min(avail);
            let gain = (n.velocity / s.velocity).clamp(0.2, 4.0);
            let start = ((n.start - PRE_ROLL_SECS).max(0.0) * rate) as usize;
            for i in 0..length {
                let j = start + i;
                if j >= len {
                    break;
                }
                let pos = i as f64 * speed;
                let k = pos as usize;
                let frac = (pos - k as f64) as f32;
                let v = s.audio[k] + frac * (s.audio[k + 1] - s.audio[k]);
                let fade = if i + release > length {
                    (length - i) as f32 / release as f32
                } else {
                    1.0
                };
                out[j] += v * gain * fade;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_label_files() {
        let notes = read_notes("0.5 1.0 52 100 0\n1.0 1.5 55\n# comment\n");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].pitch, 52);
        assert!((notes[0].velocity - 100.0 / 127.0).abs() < 1e-6);
        assert!((notes[1].velocity - 90.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn notes_play_the_nearest_sample_at_their_time() {
        let sine = |hz: f32| -> Vec<f32> {
            (0..RENDER_RATE as usize)
                .map(|i| (std::f32::consts::TAU * hz * i as f32 / RENDER_RATE as f32).sin())
                .collect()
        };
        let sampler = Sampler {
            samples: vec![Sample {
                pitch: 57,
                velocity: 1.0,
                audio: sine(220.0),
            }],
        };
        let notes = [Note { start: 0.5, end: 0.7, pitch: 69, velocity: 1.0 }];
        let out = sampler.render(&notes, RENDER_RATE as usize);
        let at = |t: f64| (t * RENDER_RATE as f64) as usize;
        assert!(out[..at(0.45)].iter().all(|v| *v == 0.0));
        assert!(out[at(0.55)..at(0.65)].iter().any(|v| v.abs() > 0.5));
        assert!(out[at(0.8)..].iter().all(|v| *v == 0.0));
        // Re-pitched an octave up: zero crossings at 440 Hz.
        let seg = &out[at(0.55)..at(0.65)];
        let crossings = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as f32 - 44.0).abs() <= 2.0, "{crossings}");
    }
}
