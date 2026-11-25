//! Learned fingering: a small Transformer tagger
//! (`scripts/train/fingering/`, 0.57M parameters) scores every string for
//! every note, then a decoder puts each chord on distinct strings with a
//! stretch cost and runs a Viterbi over hand positions between chords. On
//! true notes it puts more notes on the human's string than
//! [`super::arrange::fret_events`] (GuitarSet solo 0.55 -> 0.62, EGDB
//! 0.68 -> 0.81; `docs/transcription_roadmap.md`).
//!
//! The model is a local file ([`MODEL_FILE`] in the models folder, or
//! `GVA_FINGERING_MODEL`): it was trained on EGDB, which has no license, so
//! it is never downloaded or bundled. Six-string guitars only; everything
//! else stays on the Viterbi.
//!
//! This is a port of `scripts/train/fingering/common.py` (`features`,
//! `windowed`, `decode`); keep the two in step, the model expects exactly
//! these inputs. Notes keep their transcribed onset and end
//! ([`GridEvent::timing`]; the model loses ~0.02 string accuracy on chords
//! when every note of a chord gets the chord's onset). Differences: chords
//! are the quantizer's events instead of 50 ms onset groups, notes that
//! can't be placed are dropped (quietest first, as the Viterbi does), and
//! the Viterbi's technique rules apply (no bend on an open string, no
//! string change inside a slide or hammer-on).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tract_onnx::prelude::*;

use super::arrange::{FrettedNote, GridEvent, InstrumentSpec};

/// File name in the models folder.
pub const MODEL_FILE: &str = "fingering_tagger.onnx";
/// Recorded in the tab's metadata.
pub const ENGINE_NAME: &str = "fingering tagger (local; tract)";
pub const VITERBI_ENGINE_NAME: &str = "fingering viterbi";

const STRINGS: usize = 6;
const STANDARD: [u8; STRINGS] = [64, 59, 55, 50, 45, 40];
/// Highest fret the model knows; [`INVALID`] marks "can't be played there".
const MAX_FRET: i64 = 24;
const INVALID: i64 = MAX_FRET + 1;
const N_CONT: usize = 16;
const WINDOW: usize = 128;
const HOP: usize = 64;
/// Decoder costs (log-probability units), chosen with GuitarSet player 02
/// held out (`meta.json` "decode" of the training run).
const SPAN_COST: f64 = 1.0;
const MOVE_COST: f64 = 0.5;
/// Assignments kept per chord for the Viterbi.
const TOP: usize = 8;
/// Strings tried per note when enumerating a chord.
const CHOICES: usize = 6;
/// Same meaning as in `arrange`: a slide/hammer changing strings, a bend
/// on an open string.
const LINK_STRING_CHANGE: f64 = 4.0;
const BENT_OPEN_STRING: f64 = 6.0;
const NO_STRING: f64 = -1e9;

/// Which fingering a transcription uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fingering {
    /// The learned tagger when installed (the default), else the Viterbi.
    #[default]
    Learned,
    /// Always the Viterbi.
    Viterbi,
}

impl Fingering {
    pub fn label(self) -> &'static str {
        match self {
            Fingering::Learned => "aprendido (modelo local)",
            Fingering::Viterbi => "Viterbi",
        }
    }
}

/// `GVA_FINGERING_MODEL` if set, else the file in the models folder, when
/// it exists.
pub fn find_model() -> Option<PathBuf> {
    match std::env::var("GVA_FINGERING_MODEL") {
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => super::guitar_crnn::models_dir().map(|d| d.join(MODEL_FILE)),
    }
    .filter(|p| p.is_file())
}

/// Whether the learned model handles `spec` (six-string guitars).
pub fn supports(spec: &InstrumentSpec) -> bool {
    spec.fingering_prior && spec.tuning.len() == STRINGS
}

type Model = Arc<TypedRunnableModel>;

/// Loads (once per path) and optimizes the model, with a symbolic note
/// count.
fn model(path: &Path) -> Result<Model, String> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Model)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache = cache.lock().map_err(|_| "cache do modelo corrompido")?;
    if let Some((_, m)) = cache.iter().find(|(p, _)| p == path) {
        return Ok(m.clone());
    }
    let load = || -> TractResult<Model> {
        let mut m = tract_onnx::onnx().model_for_path(path)?;
        let t: TDim = m.symbols.sym("T").into();
        let six = TDim::from(STRINGS as i64);
        let cont = TDim::from(N_CONT as i64);
        m.set_input_fact(0, i64::fact([TDim::from(1), t.clone(), six]).into())?;
        m.set_input_fact(1, f32::fact([TDim::from(1), t, cont]).into())?;
        m.into_optimized()?.into_runnable()
    };
    let m = load().map_err(|e| format!("não foi possível carregar o modelo de dedilhado: {e}"))?;
    cache.push((path.to_path_buf(), m.clone()));
    Ok(m)
}

/// One note as the model sees it, in model order (by chord, then pitch).
#[derive(Clone, Copy, Debug)]
struct Note {
    onset: f64,
    offset: f64,
    pitch: u8,
    chord: usize,
}

/// Model inputs: frets `[N][6]` and continuous features `[N][16]`
/// (`common.features`).
fn features(notes: &[Note], tuning: &[u8]) -> (Vec<[i64; STRINGS]>, Vec<[f32; N_CONT]>) {
    let n = notes.len();
    let frets: Vec<[i64; STRINGS]> = notes
        .iter()
        .map(|note| {
            std::array::from_fn(|s| {
                let f = note.pitch as i64 - tuning[s] as i64;
                if (0..=MAX_FRET).contains(&f) { f } else { INVALID }
            })
        })
        .collect();
    let mut cont = vec![[0f32; N_CONT]; n];
    // First note and size of every chord (notes are grouped by chord).
    let mut first = vec![0usize; n];
    let mut size = vec![0usize; n];
    let mut start = 0;
    while start < n {
        let mut end = start;
        while end < n && notes[end].chord == notes[start].chord {
            end += 1;
        }
        for i in start..end {
            first[i] = start;
            size[i] = end - start;
        }
        start = end;
    }
    for i in 0..n {
        let note = &notes[i];
        let dt_prev = if i == 0 { 2.0 } else { note.onset - notes[i - 1].onset };
        let dt_next = if i + 1 == n { 2.0 } else { notes[i + 1].onset - note.onset };
        let p = note.pitch as f64;
        let pp = if i == 0 { p } else { notes[i - 1].pitch as f64 };
        let pn = if i + 1 == n { p } else { notes[i + 1].pitch as f64 };
        let c = &mut cont[i];
        c[0] = (dt_prev.clamp(0.0, 4.0) / 0.05).ln_1p() as f32;
        c[1] = (dt_next.clamp(0.0, 4.0) / 0.05).ln_1p() as f32;
        c[2] = ((note.offset - note.onset).max(0.01) / 0.25).ln().clamp(-3.0, 3.0) as f32;
        c[3] = size[i] as f32 / 6.0;
        c[4] = (i - first[i]) as f32 / 5.0;
        c[5] = if size[i] > 1 { 1.0 } else { 0.0 };
        c[6] = ((p - pp) / 12.0).clamp(-2.0, 2.0) as f32;
        c[7] = ((pn - p) / 12.0).clamp(-2.0, 2.0) as f32;
        c[8] = ((p - 64.0) / 12.0) as f32;
        for s in 0..STRINGS {
            c[9 + s] = (tuning[s] as f32 - STANDARD[s] as f32) / 2.0;
        }
        c[15] = ((note.onset - notes[first[i]].onset) / 0.05).clamp(0.0, 2.0) as f32;
    }
    (frets, cont)
}

/// Logits `[N][6]` of one window.
fn run_window(
    model: &Model,
    frets: &[[i64; STRINGS]],
    cont: &[[f32; N_CONT]],
) -> Result<Vec<[f32; STRINGS]>, String> {
    let t = frets.len();
    let f: Vec<i64> = frets.iter().flatten().copied().collect();
    let c: Vec<f32> = cont.iter().flatten().copied().collect();
    let f = Tensor::from_shape(&[1, t, STRINGS], &f).map_err(|e| e.to_string())?;
    let c = Tensor::from_shape(&[1, t, N_CONT], &c).map_err(|e| e.to_string())?;
    let out = model
        .run(tvec!(f.into(), c.into()))
        .map_err(|e| format!("modelo de dedilhado: {e}"))?;
    let view = out[0]
        .to_plain_array_view::<f32>()
        .map_err(|e| e.to_string())?;
    if view.len() != t * STRINGS {
        return Err(format!("modelo de dedilhado: saída {:?}", view.shape()));
    }
    let flat: Vec<f32> = view.iter().copied().collect();
    Ok(flat.as_chunks::<STRINGS>().0.to_vec())
}

/// Runs the model on one window of notes (frets, features) -> logits.
type WindowRunner<'a> =
    dyn FnMut(&[[i64; STRINGS]], &[[f32; N_CONT]]) -> Result<Vec<[f32; STRINGS]>, String> + 'a;

/// Logits of a whole piece over overlapping windows; each note takes the
/// window where it is farthest from an edge (`common.windowed`).
fn windowed(
    run: &mut WindowRunner<'_>,
    frets: &[[i64; STRINGS]],
    cont: &[[f32; N_CONT]],
) -> Result<Vec<[f32; STRINGS]>, String> {
    let n = frets.len();
    if n <= WINDOW {
        return run(frets, cont);
    }
    let mut out = vec![[0f32; STRINGS]; n];
    let mut best = vec![-1.0f64; n];
    let mut starts: Vec<usize> = (0..=n - WINDOW).step_by(HOP).collect();
    if *starts.last().unwrap() != n - WINDOW {
        starts.push(n - WINDOW);
    }
    for s in starts {
        let logits = run(&frets[s..s + WINDOW], &cont[s..s + WINDOW])?;
        for (k, lg) in logits.into_iter().enumerate() {
            let i = s + k;
            let mut margin = k.min(WINDOW - 1 - k) as f64;
            if i == 0 || i == n - 1 {
                margin = WINDOW as f64;
            }
            if margin > best[i] {
                out[i] = lg;
                best[i] = margin;
            }
        }
    }
    Ok(out)
}

/// Log-softmax over the strings where the note is playable (`valid`).
fn log_softmax_valid(logits: &[f32; STRINGS], valid: &[bool; STRINGS]) -> [f64; STRINGS] {
    let lg: [f64; STRINGS] =
        std::array::from_fn(|s| if valid[s] { logits[s] as f64 } else { NO_STRING });
    let m = lg.iter().copied().fold(f64::MIN, f64::max);
    let z: f64 = lg.iter().map(|v| (v - m).exp()).sum();
    std::array::from_fn(|s| lg[s] - m - z.ln())
}

/// One way to play a chord: strings (0-based) per note, score, hand
/// position (lowest fretted note).
#[derive(Clone, Debug)]
struct ChordOption {
    score: f64,
    strings: Vec<usize>,
    position: Option<i64>,
}

/// Best [`TOP`] assignments of distinct strings to one chord's notes
/// (`common.chord_options`), plus the bent-open-string rule. Empty when no
/// assignment exists.
fn chord_options(
    lp: &[[f64; STRINGS]],
    frets: &[[i64; STRINGS]],
    valid: &[[bool; STRINGS]],
    bent: &[bool],
) -> Vec<ChordOption> {
    let n = lp.len();
    let choices: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            let mut order: Vec<usize> = (0..STRINGS).filter(|&s| valid[i][s]).collect();
            order.sort_by(|&a, &b| lp[i][b].total_cmp(&lp[i][a]));
            order.truncate(CHOICES);
            order
        })
        .collect();
    let mut out = Vec::new();
    let mut current = Vec::with_capacity(n);
    fn search(
        i: usize,
        choices: &[Vec<usize>],
        current: &mut Vec<usize>,
        visit: &mut dyn FnMut(&[usize]),
    ) {
        if i == choices.len() {
            visit(current);
            return;
        }
        for &s in &choices[i] {
            if current.contains(&s) {
                continue;
            }
            current.push(s);
            search(i + 1, choices, current, visit);
            current.pop();
        }
    }
    search(0, &choices, &mut current, &mut |combo| {
        let mut score: f64 = combo.iter().enumerate().map(|(i, &s)| lp[i][s]).sum();
        let fretted: Vec<i64> = combo
            .iter()
            .enumerate()
            .map(|(i, &s)| frets[i][s])
            .filter(|&f| f > 0)
            .collect();
        let position = fretted.iter().copied().min();
        if let (Some(lo), Some(hi)) = (position, fretted.iter().copied().max()) {
            score -= SPAN_COST * (hi - lo - 3).max(0) as f64;
        }
        let bent_opens = combo
            .iter()
            .enumerate()
            .filter(|&(i, &s)| bent[i] && frets[i][s] == 0)
            .count();
        score -= BENT_OPEN_STRING * bent_opens as f64;
        out.push(ChordOption {
            score,
            strings: combo.to_vec(),
            position,
        });
    });
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(TOP);
    out
}

/// Strings and frets for every event, like [`super::arrange::fret_events`]
/// (per event, the kept `(pitch, string, fret, amplitude)` in the event's
/// pitch order), using the model at `path`. `end_secs[i]` is when event
/// `i`'s notes end. Errors when the model can't run; the caller falls back
/// to the Viterbi.
pub fn fret_events(
    path: &Path,
    events: &[GridEvent],
    end_secs: &[f64],
    spec: &InstrumentSpec,
) -> Result<Vec<Vec<FrettedNote>>, String> {
    if !supports(spec) {
        return Err(format!("{} cordas: só guitarras de 6", spec.tuning.len()));
    }
    let model = model(path)?;
    let tuning = &spec.tuning;
    let max_fret = (spec.max_fret as i64).min(MAX_FRET);
    let playable = |pitch: u8, s: usize| (0..=max_fret).contains(&(pitch as i64 - tuning[s] as i64));

    // Audio onset and end of a note: as transcribed when the event has
    // them, else the event's.
    let timing = |c: usize, pitch: u8| {
        let e = &events[c];
        let end = end_secs.get(c).copied().unwrap_or(e.secs + 0.25);
        match e.timing.iter().find(|(p, ..)| *p == pitch) {
            Some(&(_, on, off)) => (on, off.max(on)),
            None => (e.secs, end.max(e.secs)),
        }
    };
    // Per event: indices into `pitches` that some string can play, in the
    // model's order (onset, then pitch).
    let kept: Vec<Vec<usize>> = events
        .iter()
        .enumerate()
        .map(|(c, e)| {
            let mut idx: Vec<usize> = (0..e.pitches.len())
                .filter(|&k| (0..STRINGS).any(|s| playable(e.pitches[k].0, s)))
                .collect();
            idx.sort_by(|&a, &b| {
                let (pa, pb) = (e.pitches[a].0, e.pitches[b].0);
                timing(c, pa).0.total_cmp(&timing(c, pb).0).then(pa.cmp(&pb))
            });
            idx
        })
        .collect();
    let mut notes = Vec::new();
    for (c, (e, idx)) in events.iter().zip(&kept).enumerate() {
        for &k in idx {
            let pitch = e.pitches[k].0;
            let (onset, offset) = timing(c, pitch);
            notes.push(Note {
                onset,
                offset,
                pitch,
                chord: c,
            });
        }
    }
    if notes.is_empty() {
        return Ok(vec![Vec::new(); events.len()]);
    }
    let (frets, cont) = features(&notes, tuning);
    let logits = windowed(&mut |f, c| run_window(&model, f, c), &frets, &cont)?;

    // Chord options per event; when none exists (two notes only playable
    // on one string), drop the quietest note and try again.
    let mut offset = 0;
    let mut per_event: Vec<(Vec<usize>, Vec<ChordOption>)> = Vec::with_capacity(events.len());
    for (e, idx) in events.iter().zip(&kept) {
        let range = offset..offset + idx.len();
        offset += idx.len();
        let mut members: Vec<usize> = (0..idx.len()).collect();
        let options = loop {
            if members.is_empty() {
                break Vec::new();
            }
            let at = |m: &usize| range.start + m;
            let pitch = |m: &usize| e.pitches[idx[*m]].0;
            let valid: Vec<[bool; STRINGS]> = members
                .iter()
                .map(|m| std::array::from_fn(|s| playable(pitch(m), s)))
                .collect();
            let lp: Vec<[f64; STRINGS]> = members
                .iter()
                .zip(&valid)
                .map(|(m, v)| log_softmax_valid(&logits[at(m)], v))
                .collect();
            let fr: Vec<[i64; STRINGS]> = members.iter().map(|m| frets[at(m)]).collect();
            let bent: Vec<bool> = members.iter().map(|m| e.technique(pitch(m)).bend > 0).collect();
            let options = chord_options(&lp, &fr, &valid, &bent);
            if !options.is_empty() {
                break options;
            }
            // Quietest of the remaining notes.
            let quietest = (0..members.len())
                .min_by(|&a, &b| {
                    let amp = |j: usize| e.pitches[idx[members[j]]].1;
                    amp(a).total_cmp(&amp(b))
                })
                .unwrap();
            members.remove(quietest);
        };
        let members = members.iter().map(|&m| idx[m]).collect();
        per_event.push((members, options));
    }

    // Viterbi over chords: hand movement between positions (carried
    // through open-string chords), and no string change into a linked
    // note. Events without options break the chain.
    struct State {
        total: f64,
        position: Option<i64>,
        back: usize,
    }
    let mut lattice: Vec<Vec<State>> = Vec::with_capacity(events.len());
    for (i, (_, options)) in per_event.iter().enumerate() {
        let prev = i
            .checked_sub(1)
            .map(|j| &lattice[j])
            .filter(|p: &&Vec<State>| !p.is_empty());
        let linked = i > 0
            && events[i - 1].links_next()
            && per_event[i - 1].0.len() == 1
            && per_event[i].0.len() == 1;
        let row = options
            .iter()
            .map(|opt| match prev {
                None => State {
                    total: opt.score,
                    position: opt.position,
                    back: usize::MAX,
                },
                Some(prev) => {
                    let (back, total, position) = prev
                        .iter()
                        .enumerate()
                        .map(|(k, p)| {
                            let moving = match (opt.position, p.position) {
                                (Some(a), Some(b)) => (a - b).abs() as f64 * MOVE_COST,
                                _ => 0.0,
                            };
                            let from = &per_event[i - 1].1[k].strings;
                            let link = if linked && from[0] != opt.strings[0] {
                                LINK_STRING_CHANGE
                            } else {
                                0.0
                            };
                            (k, p.total + opt.score - moving - link, opt.position.or(p.position))
                        })
                        .max_by(|a, b| a.1.total_cmp(&b.1))
                        .unwrap();
                    State {
                        total,
                        position,
                        back,
                    }
                }
            })
            .collect();
        lattice.push(row);
    }
    let mut chosen = vec![usize::MAX; events.len()];
    let mut i = events.len();
    while i > 0 {
        i -= 1;
        if lattice[i].is_empty() {
            continue;
        }
        if chosen[i] == usize::MAX {
            chosen[i] = (0..lattice[i].len())
                .max_by(|&a, &b| lattice[i][a].total.total_cmp(&lattice[i][b].total))
                .unwrap();
        }
        let back = lattice[i][chosen[i]].back;
        if i > 0 && back != usize::MAX {
            chosen[i - 1] = back;
        }
    }

    Ok(events
        .iter()
        .zip(&per_event)
        .zip(chosen)
        .map(|((e, (members, options)), c)| {
            let Some(opt) = options.get(c) else {
                return Vec::new();
            };
            let mut placed: Vec<(usize, FrettedNote)> = members
                .iter()
                .zip(&opt.strings)
                .map(|(&k, &s)| {
                    let (pitch, amp) = e.pitches[k];
                    let fret = (pitch as i64 - tuning[s] as i64) as i16;
                    (k, (pitch, (s + 1) as u8, fret, amp))
                })
                .collect();
            placed.sort_by_key(|(k, _)| *k);
            placed.into_iter().map(|(_, n)| n).collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(onset: f64, pitch: u8, chord: usize) -> Note {
        Note {
            onset,
            offset: onset + 0.5,
            pitch,
            chord,
        }
    }

    #[test]
    fn features_match_the_python_definition() {
        // An E minor chord then a single G, standard tuning.
        let notes = [
            note(0.0, 40, 0),
            note(0.0, 47, 0),
            note(0.0, 52, 0),
            note(0.5, 55, 1),
        ];
        let (frets, cont) = features(&notes, &STANDARD);
        assert_eq!(frets[0], [INVALID, INVALID, INVALID, INVALID, INVALID, 0]);
        assert_eq!(frets[3], [INVALID, INVALID, 0, 5, 10, 15]);
        let close = |a: f32, b: f64| (a as f64 - b).abs() < 1e-5;
        assert!(close(cont[0][0], (2.0f64 / 0.05).ln_1p()));
        assert!(close(cont[1][0], 0.0));
        assert!(close(cont[2][1], (0.5f64 / 0.05).ln_1p()));
        assert!(close(cont[0][2], (0.5f64 / 0.25).ln()));
        assert!(close(cont[1][3], 0.5));
        assert!(close(cont[2][4], 0.4));
        assert!(close(cont[3][5], 0.0));
        assert!(close(cont[3][6], 3.0 / 12.0));
        assert!(close(cont[0][7], 7.0 / 12.0));
        assert!(close(cont[3][8], -9.0 / 12.0));
        assert!(cont.iter().all(|c| c[9..15].iter().all(|&v| v == 0.0)));
    }

    #[test]
    fn chords_get_distinct_strings_and_stretches_cost() {
        // Two notes that both prefer string 0: the second must move.
        let lp = [[0.0, -5.0, -9.0, -9.0, -9.0, -9.0], [-0.1, -0.2, -9.0, -9.0, -9.0, -9.0]];
        let frets = [[1, 6, 10, 15, 20, INVALID], [3, 8, 12, 17, 22, INVALID]];
        let valid = [[true, true, true, true, true, false]; 2];
        let options = chord_options(&lp, &frets, &valid, &[false, false]);
        assert_eq!(options[0].strings, vec![0, 1]);
        // frets 1 and 8: span 7, cost 4 * SPAN_COST.
        assert!((options[0].score - (-0.2 - 4.0 * SPAN_COST)).abs() < 1e-9);
        assert_eq!(options[0].position, Some(1));
        assert!(options.iter().all(|o| o.strings[0] != o.strings[1]));
    }

    #[test]
    fn bent_notes_avoid_open_strings() {
        let lp = [[0.0, -1.0, -9.0, -9.0, -9.0, -9.0]];
        let frets = [[0, 5, INVALID, INVALID, INVALID, INVALID]];
        let valid = [[true, true, false, false, false, false]];
        assert_eq!(chord_options(&lp, &frets, &valid, &[true])[0].strings, vec![1]);
        assert_eq!(chord_options(&lp, &frets, &valid, &[false])[0].strings, vec![0]);
    }

    #[test]
    fn windows_cover_every_note_from_the_middle() {
        let n = 300;
        let frets = vec![[0i64; STRINGS]; n];
        let cont = vec![[0f32; N_CONT]; n];
        let mut calls = Vec::new();
        let mut run = |f: &[[i64; STRINGS]], _: &[[f32; N_CONT]]| {
            calls.push(f.len());
            let id = calls.len() as f32;
            Ok(vec![[id; STRINGS]; f.len()])
        };
        let out = windowed(&mut run, &frets, &cont).unwrap();
        // Windows at 0, 64, 128 and 172.
        assert_eq!(calls, vec![WINDOW; 4]);
        assert_eq!(out[0][0], 1.0);
        assert_eq!(out[100][0], 2.0); // 36 from 64's start, 27 from 0's end
        assert_eq!(out[299][0], 4.0);
    }

    #[test]
    fn only_six_string_guitars_use_the_model() {
        let mut spec = InstrumentSpec {
            name: String::new(),
            tuning: STANDARD.to_vec(),
            tuning_name: String::new(),
            max_fret: 22,
            max_notes: 6,
            program: 29,
            fingering_prior: true,
        };
        assert!(supports(&spec));
        spec.tuning.push(35);
        assert!(!supports(&spec));
        spec.tuning = vec![43, 38, 33, 28];
        spec.fingering_prior = false;
        assert!(!supports(&spec));
    }
}
