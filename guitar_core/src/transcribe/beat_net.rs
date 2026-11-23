//! Neural beat and downbeat tracking with "Beat This!" (Foscarin, Schlüter
//! and Widmer, ISMIR 2024; code and weights MIT, CPJKU/beat_this), the
//! current state of the art across genres and the best option for solo
//! guitar, where the spectral-flux tracker in [`super::beats`] has no drums
//! to lock to. Runs the `small0` checkpoint (ONNX conversion from
//! `nilsbc/beat-this-onnx`) with its log-mel front end in tract.
//! `scripts/install_guitar_model.sh` downloads both into the models folder;
//! without them the pipeline keeps the DSP tracker.
//!
//! The network gives beat and downbeat logits at 50 frames per second. Beats
//! are its peaks (the paper's "minimal" post-processing, no DBN), repaired
//! into a regular sequence: missing beats are filled in and doubled ones
//! dropped, since the tab needs every beat. The meter (3 or 4 beats per
//! bar) and the bar phase come from the downbeat logits.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use tract_onnx::prelude::*;

use super::guitar_crnn::default_model_path;

pub const SAMPLE_RATE: u32 = 22_050;
pub const FRAME_RATE: f64 = 50.0;
const CHUNK: usize = 1500;
const BORDER: usize = 6;
const MEL_BINS: usize = 128;
pub const MODEL_FILE: &str = "beat_this_small0.onnx";
pub const FRONTEND_FILE: &str = "beat_this_logmel.onnx";
pub const ENGINE_NAME: &str = "beat-this small0 (tract)";

/// `(front end, network)` paths when both are installed next to the guitar
/// model (`GVA_BEAT_MODEL_DIR` overrides the folder).
pub fn find_models() -> Option<(PathBuf, PathBuf)> {
    let dir = match std::env::var("GVA_BEAT_MODEL_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => default_model_path()?.parent()?.to_path_buf(),
    };
    let (front, net) = (dir.join(FRONTEND_FILE), dir.join(MODEL_FILE));
    (front.is_file() && net.is_file()).then_some((front, net))
}

type Model = Arc<TypedRunnableModel>;

fn load(path: &Path, shape: &[usize]) -> Result<Model, String> {
    tract_onnx::onnx()
        .model_for_path(path)
        .and_then(|m| m.with_input_fact(0, f32::fact(shape).into()))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|e| format!("não foi possível carregar o detector de tempos: {e}"))
}

fn network(path: &Path) -> Result<Model, String> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Model)>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| "cache do modelo corrompido")?;
    if let Some((_, m)) = cache.iter().find(|(p, _)| p == path) {
        return Ok(m.clone());
    }
    let m = load(path, &[1, CHUNK, MEL_BINS])?;
    cache.push((path.to_path_buf(), m.clone()));
    Ok(m)
}

/// Log-mel frames `(frames, 128)` of mono audio at [`SAMPLE_RATE`].
fn logmel(front: &Path, audio: &[f32]) -> Result<Vec<[f32; MEL_BINS]>, String> {
    // The front end has a free input length; fix it to this clip's.
    let len = audio.len().max(2048);
    let mut padded = audio.to_vec();
    padded.resize(len, 0.0);
    let model = load(front, &[1, len])?;
    let input = Tensor::from_shape(&[1, len], &padded).map_err(|e| e.to_string())?;
    let out = model
        .run(tvec!(input.into()))
        .map_err(|e| format!("erro no detector de tempos: {e}"))?;
    let view = out[0]
        .to_plain_array_view::<f32>()
        .map_err(|e| e.to_string())?;
    let frames = view.shape()[0];
    Ok((0..frames)
        .map(|f| std::array::from_fn(|b| view[[f, b]]))
        .collect())
}

/// Frame-wise beat and downbeat logits.
#[derive(Clone, Debug, Default)]
pub struct Activations {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
}

/// Runs the network in 30 s chunks that overlap by [`BORDER`] frames, the
/// first chunk winning in overlaps, as in the reference inference.
pub fn activations(
    front: &Path,
    net: &Path,
    audio: &[f32],
    cancel: &AtomicBool,
) -> Result<Activations, String> {
    let spect = logmel(front, audio)?;
    let model = network(net)?;
    let n = spect.len();
    let step = CHUNK - 2 * BORDER;
    let mut starts: Vec<isize> = (0..)
        .map(|k| k as isize * step as isize - BORDER as isize)
        .take_while(|s| *s < n as isize - BORDER as isize)
        .collect();
    if starts.is_empty() {
        starts.push(-(BORDER as isize));
    }
    if n > step {
        *starts.last_mut().unwrap() = n as isize - (CHUNK - BORDER) as isize;
    }
    let mut act = Activations {
        beat: vec![-1000.0; n],
        downbeat: vec![-1000.0; n],
    };
    let outputs = super::parallel_map(
        starts.len(),
        4,
        &|c| {
            let start = starts[c];
            let mut chunk = vec![0f32; CHUNK * MEL_BINS];
            for k in 0..CHUNK {
                let f = start + k as isize;
                if (0..n as isize).contains(&f) {
                    chunk[k * MEL_BINS..(k + 1) * MEL_BINS].copy_from_slice(&spect[f as usize]);
                }
            }
            let input =
                Tensor::from_shape(&[1, CHUNK, MEL_BINS], &chunk).map_err(|e| e.to_string())?;
            let out = model
                .run(tvec!(input.into()))
                .map_err(|e| format!("erro no detector de tempos: {e}"))?;
            let read = |k: usize| -> Result<Vec<f32>, String> {
                let view = out[k]
                    .to_plain_array_view::<f32>()
                    .map_err(|e| e.to_string())?;
                Ok((0..CHUNK).map(|i| view[[0, i]]).collect())
            };
            Ok((read(0)?, read(1)?))
        },
        &mut |_| {},
        cancel,
    )?;
    // Earlier chunks win where chunks overlap.
    for (&start, (beat, down)) in starts.iter().zip(&outputs).rev() {
        for k in BORDER..CHUNK - BORDER {
            let f = start + k as isize;
            if (0..n as isize).contains(&f) {
                act.beat[f as usize] = beat[k];
                act.downbeat[f as usize] = down[k];
            }
        }
    }
    Ok(act)
}

/// Frames that are the maximum within ±3 frames (±60 ms) and above 0.5
/// probability, with runs of adjacent peaks merged into their mean.
pub fn peaks(logits: &[f32]) -> Vec<f64> {
    let n = logits.len();
    let raw: Vec<usize> = (0..n)
        .filter(|&i| {
            let lo = i.saturating_sub(3);
            let hi = (i + 3).min(n - 1);
            logits[i] > 0.0 && (lo..=hi).all(|j| logits[j] <= logits[i])
        })
        .collect();
    let mut out: Vec<f64> = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    for i in raw {
        if group.last().is_some_and(|&last| i - last > 1) {
            out.push(group.iter().sum::<usize>() as f64 / group.len() as f64);
            group.clear();
        }
        group.push(i);
    }
    if !group.is_empty() {
        out.push(group.iter().sum::<usize>() as f64 / group.len() as f64);
    }
    out
}

fn median(values: &[f64]) -> Option<f64> {
    let mut v = values.to_vec();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// Makes beat frames regular: drops beats much closer than the local beat
/// period (keeping the stronger one) and fills gaps of about k periods with
/// k-1 evenly spaced beats.
pub fn regularize(beats: &[f64], logits: &[f32]) -> Vec<f64> {
    if beats.len() < 4 {
        return beats.to_vec();
    }
    let strength = |f: f64| logits.get(f.round() as usize).copied().unwrap_or(-1000.0);
    let local_period = |beats: &[f64], i: usize| -> f64 {
        let lo = i.saturating_sub(8);
        let hi = (i + 8).min(beats.len() - 1);
        let gaps: Vec<f64> = beats[lo..=hi].windows(2).map(|w| w[1] - w[0]).collect();
        median(&gaps).unwrap_or(1.0)
    };
    let mut kept: Vec<f64> = Vec::with_capacity(beats.len());
    for &b in beats {
        if let Some(&last) = kept.last() {
            let period = local_period(beats, kept.len());
            if b - last < 0.6 * period {
                if strength(b) > strength(last) {
                    *kept.last_mut().unwrap() = b;
                }
                continue;
            }
        }
        kept.push(b);
    }
    let mut out = Vec::with_capacity(kept.len() * 2);
    for i in 0..kept.len() {
        if i > 0 {
            let gap = kept[i] - kept[i - 1];
            let period = local_period(&kept, i);
            let k = (gap / period).round() as usize;
            if k >= 2 && gap > 1.5 * period {
                for j in 1..k {
                    out.push(kept[i - 1] + gap * j as f64 / k as f64);
                }
            }
        }
        out.push(kept[i]);
    }
    out
}

/// Tracked beats with bar information.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatTrack {
    /// Beat times in seconds.
    pub beats: Vec<f64>,
    pub beats_per_bar: usize,
    /// Index into `beats` of the first downbeat (`< beats_per_bar`).
    pub phase: usize,
}

impl BeatTrack {
    pub fn bpm(&self) -> Option<f64> {
        let gaps: Vec<f64> = self.beats.windows(2).map(|w| w[1] - w[0]).collect();
        median(&gaps).map(|p| 60.0 / p.max(1e-3))
    }
}

/// Meter and bar phase: the `(beats per bar, phase)` whose downbeat
/// positions get the highest mean downbeat probability above the other
/// beats. 3/4 has to be clearly better than 4/4 to be chosen.
pub fn meter(beat_frames: &[f64], downbeat_logits: &[f32]) -> (usize, usize) {
    let prob = |f: f64| {
        let i = f.round() as isize;
        // Best logit within ±2 frames: downbeat peaks can sit a frame off.
        (i - 2..=i + 2)
            .filter_map(|j| usize::try_from(j).ok())
            .filter_map(|j| downbeat_logits.get(j))
            .fold(-1000f32, |m, v| m.max(*v))
            .clamp(-30.0, 30.0)
    };
    let probs: Vec<f64> = beat_frames
        .iter()
        .map(|&f| 1.0 / (1.0 + (-prob(f) as f64).exp()))
        .collect();
    let mut best = (4, 0, f64::MIN);
    for m in [4usize, 3] {
        for phase in 0..m {
            let (mut on, mut n_on, mut off, mut n_off) = (0.0, 0, 0.0, 0);
            for (i, p) in probs.iter().enumerate() {
                if i % m == phase {
                    on += p;
                    n_on += 1;
                } else {
                    off += p;
                    n_off += 1;
                }
            }
            let contrast = on / n_on.max(1) as f64 - off / n_off.max(1) as f64;
            let score = if m == 4 { contrast } else { contrast - 0.1 };
            if score > best.2 {
                best = (m, phase, score);
            }
        }
    }
    (best.0, best.1)
}

/// Beats and bars of `audio` (mono, [`SAMPLE_RATE`]).
pub fn track(
    front: &Path,
    net: &Path,
    audio: &[f32],
    cancel: &AtomicBool,
) -> Result<BeatTrack, String> {
    let act = activations(front, net, audio, cancel)?;
    Ok(track_from_activations(&act))
}

pub fn track_from_activations(act: &Activations) -> BeatTrack {
    let frames = regularize(&peaks(&act.beat), &act.beat);
    let (beats_per_bar, phase) = meter(&frames, &act.downbeat);
    BeatTrack {
        beats: frames.iter().map(|f| f / FRAME_RATE).collect(),
        beats_per_bar,
        phase: phase.min(frames.len().saturating_sub(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Logits with a peak of `height` every `period` frames from `offset`.
    fn pulses(n: usize, period: usize, offset: usize, height: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let d = (i as isize - offset as isize).rem_euclid(period as isize) as usize;
                let d = d.min(period - d) as f32;
                height - 4.0 * d
            })
            .collect()
    }

    #[test]
    fn peaks_are_local_maxima_above_half_probability() {
        let mut l = vec![-5.0f32; 40];
        l[10] = 2.0;
        l[11] = 2.0; // a flat top: one peak in the middle
        l[25] = -0.5; // below 0.5 probability
        l[30] = 1.0;
        l[32] = 3.0; // 30 isn't a maximum within ±3 frames
        assert_eq!(peaks(&l), vec![10.5, 32.0]);
    }

    #[test]
    fn regularizing_fills_missing_and_drops_doubled_beats() {
        let mut beats: Vec<f64> = (0..20).map(|i| 10.0 + i as f64 * 25.0).collect();
        beats.remove(7); // a missed beat
        beats.insert(12, beats[11] + 4.0); // a double detection
        let logits = vec![1.0f32; 1000];
        let fixed = regularize(&beats, &logits);
        assert_eq!(fixed.len(), 20, "{fixed:?}");
        for (i, b) in fixed.iter().enumerate() {
            assert!(
                (b - (10.0 + i as f64 * 25.0)).abs() < 4.5,
                "beat {i} at {b}"
            );
        }
    }

    #[test]
    fn meter_and_phase_follow_the_downbeat_logits() {
        let beats: Vec<f64> = (0..24).map(|i| 5.0 + i as f64 * 25.0).collect();
        let n = 700;
        // Downbeats on beats 1, 5, 9... (phase 1 in 4/4).
        let four = pulses(n, 100, 30, 3.0);
        assert_eq!(meter(&beats, &four), (4, 1));
        // Every third beat from beat 2: 3/4.
        let three = pulses(n, 75, 55, 3.0);
        assert_eq!(meter(&beats, &three), (3, 2));
        // No downbeat information at all: 4/4.
        assert_eq!(meter(&beats, &vec![-10.0; n]).0, 4);
    }
}
