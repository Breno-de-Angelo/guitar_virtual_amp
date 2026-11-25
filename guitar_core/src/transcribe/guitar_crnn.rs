//! Guitar-specific note transcription with the high-resolution CRNN of
//! Kong et al. ("High-resolution Piano Transcription with Pedals by
//! Regressing Onset and Offset Times", 2021), fine-tuned for guitar by Riley
//! et al. on the GAPS dataset ("GAPS: A Large and Diverse Classical Guitar
//! Dataset and Benchmark Transcription Model", ISMIR 2024; weights MIT,
//! `xavriley/midi-transcription-models` on Hugging Face). Zero-shot on
//! GuitarSet it reaches ~88% note F1 (50 ms), against ~66% for Basic Pitch
//! on unseen guitar data.
//!
//! The model is too big to bundle (~99 MB): `scripts/install_transcription_models.sh`
//! downloads the PyTorch checkpoint and exports it to ONNX (log-mel front
//! end included) at [`default_model_path`]. When it isn't there, the
//! pipeline falls back to Basic Pitch.
//!
//! The model reads 10 s windows of 16 kHz audio and returns, at 100 frames
//! per second and for 88 pitches, onset and offset *regression* curves
//! (peaks mark the exact event time, with sub-frame precision), frame
//! activity and velocity. Notes are read from them as in the reference
//! `RegressionPostProcessor`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use tract_onnx::prelude::*;

use super::basic_pitch::NoteEstimate;
use crate::shared::paths::app_data_dir;

pub const SAMPLE_RATE: u32 = 16_000;
const SEGMENT: usize = 10 * SAMPLE_RATE as usize;
/// Frames per segment (`center = true` adds one at the end).
const SEGMENT_FRAMES: usize = 1001;
pub const FRAME_RATE: f64 = 100.0;
/// Frames dropped at each inner window edge, where the recurrent layers see
/// little context: 1 s, so 8 s are kept per 10 s window. (The reference
/// code keeps 5 s; this runs 40% fewer windows with the same GuitarSet F1.)
const MARGIN_FRAMES: usize = 100;
/// Each window in flight takes ~600 MB in tract (im2col buffers of the
/// convolutions), and more threads barely help past 4 (memory bound).
const MAX_THREADS: usize = 4;
const PITCHES: usize = 88;
const LOWEST_MIDI: u8 = 21;
pub const MODEL_FILE: &str = "guitar_gaps_crnn.onnx";
/// Name recorded in the tab's metadata.
pub const ENGINE_NAME: &str = "guitar CRNN (Riley et al., GAPS 2024; tract)";

/// `GVA_GUITAR_MODEL` if set, else `<data dir>/guitar_virtual_amp/tools/models/`.
pub fn default_model_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GVA_GUITAR_MODEL") {
        return Some(PathBuf::from(path));
    }
    models_dir().map(|d| d.join(MODEL_FILE))
}

/// Where `scripts/install_transcription_models.sh` puts the optional
/// models (the guitar CRNN, Beat This!).
pub fn models_dir() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("tools").join("models"))
}

/// The installed model, if any.
pub fn find_model() -> Option<PathBuf> {
    default_model_path().filter(|p| p.is_file())
}

type Model = Arc<TypedRunnableModel>;

/// Loads (once per path) and optimizes the model; takes a second or two.
fn model(path: &Path) -> Result<Model, String> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Model)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache = cache.lock().map_err(|_| "cache do modelo corrompido")?;
    if let Some((_, m)) = cache.iter().find(|(p, _)| p == path) {
        return Ok(m.clone());
    }
    let m = tract_onnx::onnx()
        .model_for_path(path)
        .and_then(|m| m.with_input_fact(0, f32::fact([1, SEGMENT]).into()))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|e| format!("não foi possível carregar o modelo de guitarra: {e}"))?;
    cache.push((path.to_path_buf(), m.clone()));
    Ok(m)
}

/// Frame-level model output for a whole clip, frame `i` at `i / 100` s.
#[derive(Clone, Debug, Default)]
pub struct Posteriors {
    pub onset: Vec<[f32; PITCHES]>,
    pub offset: Vec<[f32; PITCHES]>,
    pub frame: Vec<[f32; PITCHES]>,
    pub velocity: Vec<[f32; PITCHES]>,
}

impl Posteriors {
    pub fn len(&self) -> usize {
        self.frame.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frame.is_empty()
    }
}

type SegmentOutput = [Vec<[f32; PITCHES]>; 4];

fn run_segment(model: &Model, audio: &[f32], start: usize) -> Result<SegmentOutput, String> {
    let mut window = vec![0f32; SEGMENT];
    if start < audio.len() {
        let end = (start + SEGMENT).min(audio.len());
        window[..end - start].copy_from_slice(&audio[start..end]);
    }
    let input = Tensor::from_shape(&[1, SEGMENT], &window).map_err(|e| e.to_string())?;
    let result = model
        .run(tvec!(input.into()))
        .map_err(|e| format!("erro no modelo de guitarra: {e}"))?;
    let mut out: SegmentOutput = Default::default();
    for (k, slot) in out.iter_mut().enumerate() {
        let view = result[k]
            .to_plain_array_view::<f32>()
            .map_err(|e| e.to_string())?;
        let frames = view.shape()[1];
        *slot = (0..frames)
            .map(|f| std::array::from_fn(|p| view[[0, f, p]]))
            .collect();
    }
    Ok(out)
}

/// Runs the model over `audio` (mono, [`SAMPLE_RATE`]), on several threads.
/// Consecutive windows overlap by two margins; each keeps its middle part.
pub fn posteriors(
    model_path: &Path,
    audio: &[f32],
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Posteriors, String> {
    let model = model(model_path)?;
    let margin = MARGIN_FRAMES;
    let keep = SEGMENT_FRAMES - 1; // the extra `center` frame is dropped
    let hop = (keep - 2 * margin) * (SAMPLE_RATE as usize / FRAME_RATE as usize);
    let segments = if audio.len() <= SEGMENT {
        1
    } else {
        (audio.len() - SEGMENT).div_ceil(hop) + 1
    };
    let results = super::parallel_map(
        segments,
        MAX_THREADS,
        &|i| run_segment(&model, audio, i * hop),
        progress,
        cancel,
    )?;
    let total_frames = (audio.len() as f64 / SAMPLE_RATE as f64 * FRAME_RATE).ceil() as usize + 1;
    let mut post = Posteriors::default();
    for (i, segment) in results.into_iter().enumerate() {
        let [onset, offset, frame, velocity] = segment;
        let from = if i == 0 { 0 } else { margin };
        let to = if i == segments - 1 {
            keep
        } else {
            keep - margin
        }
        .min(onset.len());
        post.onset.extend_from_slice(&onset[from..to]);
        post.offset.extend_from_slice(&offset[from..to]);
        post.frame.extend_from_slice(&frame[from..to]);
        post.velocity.extend_from_slice(&velocity[from..to]);
    }
    for v in [
        &mut post.onset,
        &mut post.offset,
        &mut post.frame,
        &mut post.velocity,
    ] {
        v.truncate(total_frames);
    }
    Ok(post)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrnnParams {
    pub onset_threshold: f32,
    pub offset_threshold: f32,
    pub frame_threshold: f32,
    /// Shortest note kept, in seconds (TART drops anything under 30 ms).
    pub min_duration: f64,
    pub min_pitch: u8,
    pub max_pitch: u8,
}

impl CrnnParams {
    pub fn guitar() -> Self {
        Self {
            onset_threshold: 0.3,
            offset_threshold: 0.3,
            frame_threshold: 0.1,
            min_duration: 0.03,
            min_pitch: 28,
            max_pitch: 92,
        }
    }
}

/// Peaks of a regression curve above `threshold` that rise and fall
/// monotonically over `neighbour` frames on each side, with their sub-frame
/// shift (Kong et al., Section III-D).
fn regression_peaks(x: &[f32], threshold: f32, neighbour: usize) -> Vec<(usize, f64)> {
    let n = x.len();
    let mut out = Vec::new();
    if n <= 2 * neighbour {
        return out;
    }
    for i in neighbour..n - neighbour {
        if x[i] <= threshold {
            continue;
        }
        let monotonic =
            (0..neighbour).all(|k| x[i - k] >= x[i - k - 1] && x[i + k] >= x[i + k + 1]);
        if !monotonic {
            continue;
        }
        let (prev, cur, next) = (x[i - 1] as f64, x[i] as f64, x[i + 1] as f64);
        let denom = if prev > next { cur - next } else { cur - prev };
        let shift = if denom.abs() > 1e-9 {
            (next - prev) / denom / 2.0
        } else {
            0.0
        };
        out.push((i, shift.clamp(-0.5, 0.5)));
    }
    out
}

/// Notes from the posteriors: each onset peak starts a note, which ends at
/// the offset peak or where the frame activity drops (whichever the
/// reference rule picks), or at the next onset of the same pitch.
pub fn extract_notes(post: &Posteriors, params: &CrnnParams) -> Vec<NoteEstimate> {
    let n = post.len();
    let lo = params.min_pitch.saturating_sub(LOWEST_MIDI) as usize;
    let hi = (params.max_pitch.saturating_sub(LOWEST_MIDI) as usize).min(PITCHES - 1);
    let mut notes = Vec::new();
    if n < 10 {
        return notes;
    }
    for p in lo..=hi {
        let onset_curve: Vec<f32> = post.onset.iter().map(|f| f[p]).collect();
        let offset_curve: Vec<f32> = post.offset.iter().map(|f| f[p]).collect();
        let onsets = regression_peaks(&onset_curve, params.onset_threshold, 2);
        if onsets.is_empty() {
            continue;
        }
        let mut is_onset = vec![None; n];
        for &(i, s) in &onsets {
            is_onset[i] = Some(s);
        }
        let mut is_offset = vec![None; n];
        for (i, s) in regression_peaks(&offset_curve, params.offset_threshold, 4) {
            is_offset[i] = Some(s);
        }
        let mut push = |bgn: usize, onset_shift: f64, fin: usize, offset_shift: f64| {
            let start = (bgn as f64 + onset_shift) / FRAME_RATE;
            let end = (fin as f64 + offset_shift) / FRAME_RATE;
            if end - start >= params.min_duration {
                let frames = &post.frame[bgn..=fin.min(n - 1)];
                let activity =
                    frames.iter().map(|f| f[p]).sum::<f32>() / frames.len().max(1) as f32;
                notes.push(NoteEstimate {
                    start,
                    end,
                    pitch: p as u8 + LOWEST_MIDI,
                    // Loudness for the arrangement: the model's velocity,
                    // tempered by how clearly the note sounds.
                    amplitude: (post.velocity[bgn][p] * 0.5 + activity * 0.5).clamp(0.0, 1.0),
                });
            }
        };
        // (start frame, its shift, frame where activity vanished, offset peak)
        let mut current: Option<(usize, f64)> = None;
        let mut frame_gone: Option<usize> = None;
        let mut offset_at: Option<(usize, f64)> = None;
        for i in 0..n {
            if let Some(shift) = is_onset[i] {
                if let Some((bgn, s)) = current {
                    // Re-struck before the previous note ended.
                    push(bgn, s, i.saturating_sub(1), 0.0);
                }
                current = Some((i, shift));
                frame_gone = None;
                offset_at = None;
                continue;
            }
            let Some((bgn, s)) = current else {
                continue;
            };
            if post.frame[i][p] <= params.frame_threshold && frame_gone.is_none() {
                frame_gone = Some(i);
            }
            if let (Some(shift), None) = (is_offset[i], offset_at) {
                offset_at = Some((i, shift));
            }
            if let Some(gone) = frame_gone {
                let (fin, fin_shift) = match offset_at {
                    Some((off, shift)) if off - bgn > gone - off => (off, shift),
                    _ => (gone, is_offset[gone].unwrap_or(0.0)),
                };
                push(bgn, s, fin, fin_shift);
                current = None;
                frame_gone = None;
                offset_at = None;
            } else if i - bgn >= 600 || i == n - 1 {
                push(bgn, s, i, is_offset[i].unwrap_or(0.0));
                current = None;
                offset_at = None;
            }
        }
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_peaks_have_sub_frame_shifts() {
        // A symmetric peak sits exactly on its frame; a skewed one leans
        // toward the higher neighbour.
        let sym = [0.0, 0.1, 0.4, 0.9, 0.4, 0.1, 0.0];
        assert_eq!(regression_peaks(&sym, 0.3, 2), vec![(3, 0.0)]);
        let skew = [0.0, 0.1, 0.5, 0.9, 0.3, 0.1, 0.0];
        let peaks = regression_peaks(&skew, 0.3, 2);
        assert_eq!(peaks.len(), 1);
        assert!(peaks[0].1 < 0.0, "{peaks:?}");
        // Below threshold or not monotonic: nothing.
        assert!(regression_peaks(&[0.0, 0.1, 0.2, 0.25, 0.2, 0.1, 0.0], 0.3, 2).is_empty());
        assert!(regression_peaks(&[0.0, 0.5, 0.2, 0.9, 0.4, 0.1, 0.0], 0.3, 2).is_empty());
    }

    fn empty(n: usize) -> Posteriors {
        Posteriors {
            onset: vec![[0.0; PITCHES]; n],
            offset: vec![[0.0; PITCHES]; n],
            frame: vec![[0.0; PITCHES]; n],
            velocity: vec![[0.0; PITCHES]; n],
        }
    }

    fn bump(curve: &mut [[f32; PITCHES]], at: usize, p: usize) {
        for (k, v) in [(0, 1.0), (1, 0.6), (2, 0.3)] {
            curve[at + k][p] = curve[at + k][p].max(v);
            curve[at - k][p] = curve[at - k][p].max(v);
        }
    }

    #[test]
    fn notes_follow_onsets_offsets_and_frames() {
        let mut post = empty(300);
        let p = 45 - LOWEST_MIDI as usize;
        // A2 from frame 20, offset peak at 80, frames active until 82.
        bump(&mut post.onset, 20, p);
        bump(&mut post.offset, 80, p);
        for f in &mut post.frame[20..83] {
            f[p] = 0.9;
        }
        post.velocity[20][p] = 0.7;
        // Same pitch struck again at 150, activity fades at 200 (no offset).
        bump(&mut post.onset, 150, p);
        for f in &mut post.frame[150..200] {
            f[p] = 0.8;
        }
        // A blip of 2 frames is too short to be a note.
        let q = 60 - LOWEST_MIDI as usize;
        bump(&mut post.onset, 250, q);
        post.frame[250][q] = 0.9;
        let notes = extract_notes(&post, &CrnnParams::guitar());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(notes[0].pitch, 45);
        assert!((notes[0].start - 0.20).abs() < 1e-6);
        assert!((notes[0].end - 0.80).abs() < 1e-6, "{}", notes[0].end);
        assert!((notes[1].start - 1.50).abs() < 1e-6);
        assert!((notes[1].end - 2.00).abs() < 1e-6, "{}", notes[1].end);
    }
}
