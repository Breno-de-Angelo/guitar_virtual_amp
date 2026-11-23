//! Polyphonic note transcription with Spotify's Basic Pitch model
//! (Apache-2.0, see `assets/models/README.md`), run in pure Rust by
//! `tract-onnx`, so it works offline and on every platform the app builds for.
//!
//! The model looks at 2 s windows of 22.05 kHz audio and returns, for 172
//! frames x 88 pitches, the probability that a note starts (onset) and that
//! a note sounds (frame). Windows overlap by 30 frames and the overlapping
//! halves are dropped, as in the reference implementation. Notes are then
//! read from the posteriors: onset peaks, extended while the frame
//! posterior stays high.

use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use tract_onnx::prelude::*;

pub const SAMPLE_RATE: u32 = 22_050;
const MODEL_BYTES: &[u8] = include_bytes!("../../assets/models/basic_pitch_nmp.onnx");
const WINDOW: usize = 43_844;
const FRAMES: usize = 172;
const OVERLAP_FRAMES: usize = 30;
const OVERLAP: usize = OVERLAP_FRAMES * FFT_HOP;
const HOP: usize = WINDOW - OVERLAP;
const PITCHES: usize = 88;
const LOWEST_MIDI: u8 = 21;
const ONSET_OUTPUT: usize = 0;
const NOTE_OUTPUT: usize = 1;
/// Hop of the model's CQT, in samples.
const FFT_HOP: usize = 256;
/// Frames per second of the posteriors.
pub const FRAME_RATE: f64 = SAMPLE_RATE as f64 / FFT_HOP as f64;

type Model = Arc<TypedRunnableModel>;

fn model() -> Result<&'static Model, String> {
    static MODEL: OnceLock<Result<Model, String>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            tract_onnx::onnx()
                .model_for_read(&mut std::io::Cursor::new(MODEL_BYTES))
                .and_then(|m| m.with_input_fact(0, f32::fact([1, WINDOW, 1]).into()))
                .and_then(|m| m.into_optimized())
                .and_then(|m| m.into_runnable())
                .map_err(|e| format!("não foi possível carregar o modelo Basic Pitch: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Frame-level model output for a whole clip.
#[derive(Clone, Debug, Default)]
pub struct Posteriors {
    /// Time (seconds) of each frame.
    pub times: Vec<f64>,
    pub onsets: Vec<[f32; PITCHES]>,
    pub frames: Vec<[f32; PITCHES]>,
}

/// Runs the model over `audio` (mono, [`SAMPLE_RATE`]). `progress` gets
/// 0..=1; setting `cancel` stops early with an error.
pub fn posteriors(
    audio: &[f32],
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Posteriors, String> {
    let model = model()?;
    let duration = audio.len() as f64 / SAMPLE_RATE as f64;
    let mut padded = vec![0f32; OVERLAP / 2];
    padded.extend_from_slice(audio);
    let windows = padded.len().div_ceil(HOP).max(1);
    // Frames are FFT_HOP apart inside a window (not WINDOW / FRAMES: see
    // spotify/basic-pitch#190); each window's frames are timed from its own
    // start, so the window hop mismatch doesn't accumulate.
    let frame_samples = FFT_HOP as f64;
    let run_window = |w: usize| -> Result<Posteriors, String> {
        let start = w * HOP;
        let mut window = vec![0f32; WINDOW];
        let end = (start + WINDOW).min(padded.len());
        if start < end {
            window[..end - start].copy_from_slice(&padded[start..end]);
        }
        let input: Tensor = tract_ndarray::Array3::from_shape_vec((1, WINDOW, 1), window)
            .map_err(|e| e.to_string())?
            .into();
        let result = model
            .run(tvec!(input.into()))
            .map_err(|e| format!("erro no Basic Pitch: {e}"))?;
        let onsets = result[ONSET_OUTPUT]
            .to_plain_array_view::<f32>()
            .map_err(|e| e.to_string())?;
        let frames = result[NOTE_OUTPUT]
            .to_plain_array_view::<f32>()
            .map_err(|e| e.to_string())?;
        let mut out = Posteriors::default();
        for k in OVERLAP_FRAMES / 2..FRAMES - OVERLAP_FRAMES / 2 {
            let t = (start as f64 + k as f64 * frame_samples - (OVERLAP / 2) as f64)
                / SAMPLE_RATE as f64;
            if t < 0.0 || t > duration {
                continue;
            }
            out.times.push(t);
            out.onsets.push(std::array::from_fn(|p| onsets[[0, k, p]]));
            out.frames.push(std::array::from_fn(|p| frames[[0, k, p]]));
        }
        Ok(out)
    };
    let parts = super::parallel_map(windows, 8, &run_window, progress, cancel)?;
    let mut out = Posteriors::default();
    for part in parts {
        out.times.extend(part.times);
        out.onsets.extend(part.onsets);
        out.frames.extend(part.frames);
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteParams {
    pub onset_threshold: f32,
    pub frame_threshold: f32,
    /// Shortest note kept, in frames (~11.6 ms each).
    pub min_frames: usize,
    /// Frames below `frame_threshold` tolerated inside a note.
    pub energy_tolerance: usize,
    pub min_pitch: u8,
    pub max_pitch: u8,
}

impl NoteParams {
    pub fn guitar() -> Self {
        Self {
            onset_threshold: 0.5,
            frame_threshold: 0.3,
            min_frames: 5,
            energy_tolerance: 11,
            min_pitch: 30,
            max_pitch: 90,
        }
    }

    pub fn bass() -> Self {
        Self {
            onset_threshold: 0.5,
            frame_threshold: 0.3,
            min_frames: 6,
            energy_tolerance: 11,
            min_pitch: 23,
            max_pitch: 67,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEstimate {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    /// Mean frame posterior over the note, 0..1.
    pub amplitude: f32,
}

/// Turns posteriors into notes: every onset peak above the threshold starts
/// a note that lasts while the frame posterior stays above its threshold
/// (with `energy_tolerance` frames of slack). Later onsets are processed
/// first so a re-struck note cuts the previous one, like the reference code.
pub fn extract_notes(post: &Posteriors, params: &NoteParams) -> Vec<NoteEstimate> {
    let n = post.times.len();
    if n < 3 {
        return Vec::new();
    }
    let lo = params.min_pitch.saturating_sub(LOWEST_MIDI) as usize;
    let hi = (params.max_pitch.saturating_sub(LOWEST_MIDI) as usize).min(PITCHES - 1);
    let mut remaining: Vec<[f32; PITCHES]> = post.frames.clone();
    let mut peaks = Vec::new();
    for t in 1..n - 1 {
        for p in lo..=hi {
            let o = post.onsets[t][p];
            if o >= params.onset_threshold
                && o > post.onsets[t - 1][p]
                && o >= post.onsets[t + 1][p]
            {
                peaks.push((t, p));
            }
        }
    }
    peaks.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut notes = Vec::new();
    for (t, p) in peaks {
        let (mut i, mut below) = (t + 1, 0);
        while i < n && below < params.energy_tolerance {
            if remaining[i][p] < params.frame_threshold {
                below += 1;
            } else {
                below = 0;
            }
            i += 1;
        }
        let end = i - below;
        if end <= t + params.min_frames {
            continue;
        }
        let amplitude = remaining[t..end].iter().map(|f| f[p]).sum::<f32>() / (end - t) as f32;
        for frame in &mut remaining[t..end] {
            frame[p] = 0.0;
        }
        let end_time = post.times.get(end).copied().unwrap_or(post.times[n - 1]);
        notes.push(NoteEstimate {
            start: post.times[t],
            end: end_time,
            pitch: p as u8 + LOWEST_MIDI,
            amplitude,
        });
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    // A double onset peak on one attack: merge re-starts of the same pitch
    // that come less than ~one 32nd at fast tempos (60 ms) later.
    let mut merged: Vec<NoteEstimate> = Vec::with_capacity(notes.len());
    for note in notes {
        let previous = merged
            .iter_mut()
            .rev()
            .take_while(|m| note.start - m.start < 0.06)
            .find(|m| m.pitch == note.pitch);
        match previous {
            Some(prev) => {
                prev.end = prev.end.max(note.end);
                prev.amplitude = prev.amplitude.max(note.amplitude);
            }
            None => merged.push(note),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pluck(pitches: &[(u8, f64)], secs: f64) -> Vec<f32> {
        let n = (secs * SAMPLE_RATE as f64) as usize;
        let mut out = vec![0f32; n];
        for &(pitch, start) in pitches {
            let f = 440.0 * 2f64.powf((pitch as f64 - 69.0) / 12.0);
            let first = (start * SAMPLE_RATE as f64) as usize;
            for (i, sample) in out.iter_mut().enumerate().skip(first) {
                let t = (i - first) as f64 / SAMPLE_RATE as f64;
                let env = (-t * 3.0).exp();
                let mut v = 0.0;
                for h in 1..=8 {
                    v += (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64;
                }
                *sample += (0.2 * env * v) as f32;
            }
        }
        out
    }

    #[test]
    fn transcribes_plucked_notes_with_timing() {
        let audio = pluck(&[(45, 0.5), (52, 1.5), (57, 2.5)], 4.0);
        let post = posteriors(&audio, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert!(
            (post.times.len() as f64 - 4.0 * FRAME_RATE).abs() < 3.0,
            "{} frames",
            post.times.len()
        );
        let notes = extract_notes(&post, &NoteParams::guitar());
        for (pitch, start) in [(45u8, 0.5f64), (52, 1.5), (57, 2.5)] {
            assert!(
                notes
                    .iter()
                    .any(|n| n.pitch == pitch && (n.start - start).abs() < 0.05),
                "missing {pitch} at {start}: {notes:?}"
            );
        }
    }

    #[test]
    fn cancelling_stops_inference() {
        let cancel = AtomicBool::new(true);
        assert!(posteriors(&[0.0; 50_000], &mut |_| {}, &cancel).is_err());
    }

    #[test]
    fn note_extraction_follows_onsets_and_frames() {
        // Synthetic posteriors: pitch index 24 (MIDI 45) on for frames 10..40.
        let n = 60;
        let mut post = Posteriors {
            times: (0..n).map(|i| i as f64 / FRAME_RATE).collect(),
            onsets: vec![[0.0; PITCHES]; n],
            frames: vec![[0.0; PITCHES]; n],
        };
        post.onsets[10][24] = 0.9;
        for f in &mut post.frames[10..40] {
            f[24] = 0.8;
        }
        // A lone onset with no sustain is dropped.
        post.onsets[50][30] = 0.9;
        let notes = extract_notes(&post, &NoteParams::guitar());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].pitch, 45);
        assert!((notes[0].end - 40.0 / FRAME_RATE).abs() < 1e-9);
        assert!((notes[0].amplitude - 0.8).abs() < 1e-6);
    }
}
