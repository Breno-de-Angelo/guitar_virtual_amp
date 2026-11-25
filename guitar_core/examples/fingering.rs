//! String and fret assignment with the app's fingering Viterbi
//! (`transcribe::arrange::fret_events_with`, default costs and the GuitarSet
//! string prior), for benchmarking against other methods from Python
//! (`scripts/train/bench_fingering.py`).
//!
//! Reads one JSON object per line on stdin:
//! `{"tuning": [64, 59, 55, 50, 45, 40], "notes": [[onset_secs, pitch], ...]}`
//! (open strings from string 1, the highest) and writes one line per input:
//! `[[string, fret], ...]`, index for index with `notes` (`[0, -1]` when a
//! note got no position). Notes starting within 100 ms are grouped into one
//! chord, as in `eval_fingering`.
//!
//! With `--learned MODEL.onnx` it runs the learned tagger
//! (`transcribe::fingering_net`) instead: notes may carry their offset as a
//! third value (`[onset, pitch, offset]`), and chords are notes within 50 ms
//! of the chord's first note (as `scripts/train/fingering/common.py`).
//!
//! ```text
//! cargo run --release --example fingering [-- --learned MODEL.onnx] < cases.jsonl > frets.jsonl
//! ```

use std::io::{BufRead, Write};

use guitar_core::transcribe::arrange::{
    FingeringCosts, GridEvent, InstrumentSpec, StringPrior, fret_events_with,
};
use guitar_core::transcribe::fingering_net;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let learned = args
        .iter()
        .position(|a| a == "--learned")
        .map(|i| std::path::PathBuf::from(args.get(i + 1).expect("--learned MODEL.onnx")));
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let case: serde_json::Value = serde_json::from_str(&line).expect("JSON case");
        let tuning: Vec<u8> = case["tuning"]
            .as_array()
            .expect("tuning")
            .iter()
            .map(|v| v.as_u64().expect("tuning pitch") as u8)
            .collect();
        let notes: Vec<(f64, u8)> = case["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .map(|n| {
                (
                    n[0].as_f64().expect("onset"),
                    n[1].as_u64().expect("pitch") as u8,
                )
            })
            .collect();
        let offsets: Vec<f64> = case["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .map(|n| n.get(2).and_then(|v| v.as_f64()).unwrap_or(n[0].as_f64().unwrap() + 0.25))
            .collect();
        let spec = InstrumentSpec {
            name: String::new(),
            max_notes: tuning.len(),
            tuning,
            tuning_name: String::new(),
            max_fret: 24,
            program: 25,
            fingering_prior: true,
        };
        // Group into chords: index lists, onsets within 100 ms, one note per pitch.
        let mut order: Vec<usize> = (0..notes.len()).collect();
        order.sort_by(|&a, &b| notes[a].0.total_cmp(&notes[b].0));
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for i in order {
            match groups.last_mut() {
                Some(g) if learned.is_some() && notes[i].0 - notes[g[0]].0 <= 0.05 => g.push(i),
                Some(g)
                    if learned.is_none()
                        && notes[i].0 - notes[g[0]].0 <= 0.1
                        && !g.iter().any(|&j| notes[j].1 == notes[i].1) =>
                {
                    g.push(i)
                }
                _ => groups.push(vec![i]),
            }
        }
        let events: Vec<GridEvent> = groups
            .iter()
            .map(|g| {
                let t = notes[g[0]].0;
                GridEvent {
                    tick: (t * 1920.0) as i64,
                    end_tick: (t * 1920.0) as i64 + 240,
                    secs: t,
                    pitches: g.iter().map(|&i| (notes[i].1, 0.8)).collect(),
                    techniques: Vec::new(),
                    timing: g.iter().map(|&i| (notes[i].1, notes[i].0, offsets[i])).collect(),
                }
            })
            .collect();
        let mut result = vec![(0u8, -1i16); notes.len()];
        if let Some(model) = &learned {
            let ends: Vec<f64> = groups
                .iter()
                .map(|g| g.iter().map(|&i| offsets[i]).fold(f64::MIN, f64::max))
                .collect();
            let frets =
                fingering_net::fret_events(model, &events, &ends, &spec).expect("learned fingering");
            // Equal pitches in one chord (unisons) take the strings in order.
            for (g, placed) in groups.iter().zip(&frets) {
                let mut used = vec![false; placed.len()];
                for &i in g {
                    if let Some(k) = (0..placed.len()).find(|&k| !used[k] && placed[k].0 == notes[i].1) {
                        used[k] = true;
                        result[i] = (placed[k].1, placed[k].2);
                    }
                }
            }
        } else {
            let frets = fret_events_with(
                &events,
                &spec,
                &FingeringCosts::default(),
                Some(StringPrior::guitarset()),
            );
            for (g, placed) in groups.iter().zip(&frets) {
                for &i in g {
                    if let Some((_, s, f, _)) = placed.iter().find(|(p, ..)| *p == notes[i].1) {
                        result[i] = (*s, *f);
                    }
                }
            }
        }
        let row: Vec<[i64; 2]> = result.iter().map(|(s, f)| [*s as i64, *f as i64]).collect();
        writeln!(out, "{}", serde_json::to_string(&row).expect("JSON")).expect("stdout");
    }
}
