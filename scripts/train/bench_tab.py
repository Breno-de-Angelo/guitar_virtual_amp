"""Tablature benchmark: string-fret assignment and audio-to-tablature, on the
sets with string labels (GuitarSet players 03-05, EGDB test clips).

Methods:
- Fingering from the true notes (string accuracy, as `eval_fingering`):
  `viterbi` (the app's fingering, `cargo run --release --example fingering`)
  and `lowest` (every note at its lowest fret, chords on distinct strings).
- Modular audio-to-tab: notes cached by `bench_notes.py` (`--notes-engines`,
  e.g. crnn-v2_spark_s5000; guitar-class notes for multi-instrument models)
  placed by the app's Viterbi.

Sets: EGSet12 (real electric guitar, miked amp; `--egset12`), GuitarSet 03-05 comp/solo, EGDB test clips (DI and three amp tones),
and the EGDB Marshall clips inside band mixes: raw mix, and the guitar stems
of `bench_separation.py` (htdemucs_6s, BS-RoFormer SW).
- End-to-end: TabCNN (Wiggins & Kim 2019; ONNX exports `cstr/tabcnn-onnx`,
  CC BY 4.0): `tabcnn` (GuitarSet) and `tabcnn-gpfx` (GuitarSet +
  GuitarProFX tones, Pedroza et al. 2024). Both were trained on GuitarSet,
  possibly including players 03-05, so their GuitarSet scores may be
  optimistic; EGDB is unseen by both.

Metrics, mean per item: F50 (onset 50 ms + pitch, mir_eval), Tab F1 (F50
match on the same string, so also the same fret), and for the fingering-only
methods the share of true notes on the reference string.

Usage:
  python bench_tab.py --data PREPARED --guitarset GUITARSET --egdb EGDB --bench BENCH \\
      --fingering-bin target/release/examples/fingering --tabcnn DIR [--limit 12]
"""
import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench_notes import hz, spread  # noqa: E402

STANDARD = [64, 59, 55, 50, 45, 40]  # string 1 (high e) .. 6 (low E)


# ---------------------------------------------------------------- truth with strings

def guitarset_tab(jams):
    doc = json.load(open(jams))
    out = []
    for a in doc["annotations"]:
        if a["namespace"] == "note_midi":
            string = 6 - int(a["annotation_metadata"]["data_source"])  # source 0 = low E
            for n in a["data"]:
                p = int(round(n["value"]))
                out.append((n["time"], n["time"] + n["duration"], p, string))
    return sorted(out)


def egdb_tab(midi_path):
    """EGDB labels: one MIDI track per string, MIDI channel 0 (high e) to 5
    (track names, when present, say the same); the tempo map is in the first
    track."""
    import mido

    m = mido.MidiFile(midi_path)
    tempos = []  # (tick, tempo)
    for track in m.tracks:
        tick = 0
        for msg in track:
            tick += msg.time
            if msg.type == "set_tempo":
                tempos.append((tick, msg.tempo))
    tempos = sorted(tempos) or [(0, 500000)]
    if tempos[0][0] > 0:
        tempos.insert(0, (0, 500000))

    def secs(tick):
        t, last_tick, tempo = 0.0, 0, tempos[0][1]
        for at, new in tempos[1:]:
            if at >= tick:
                break
            t += mido.tick2second(at - last_tick, m.ticks_per_beat, tempo)
            last_tick, tempo = at, new
        return t + mido.tick2second(tick - last_tick, m.ticks_per_beat, tempo)

    out = []
    for track in m.tracks:
        tick, on = 0, {}
        for msg in track:
            tick += msg.time
            if msg.type == "note_on" and msg.velocity > 0:
                on[(msg.channel, msg.note)] = tick
            elif msg.type in ("note_off", "note_on") and (msg.channel, msg.note) in on:
                start = on.pop((msg.channel, msg.note))
                if msg.channel < 6:
                    out.append((secs(start), secs(tick), msg.note, msg.channel + 1))
    return sorted(out)


def sets(data, guitarset, egdb, limit, egset12=None):
    groups = defaultdict(list)
    if egset12:
        for jams in sorted(egset12.glob("*.jams")):
            groups["egset12"].append({"audio": str(jams.with_suffix(".wav")), "group": "egset12", "truth": ("gs", str(jams))})
    for jams in sorted((guitarset / "annotation").glob("0[345]_*.jams")):
        audio = guitarset / "audio" / (jams.stem + "_mic.wav")
        g = "guitarset/" + ("comp" if jams.stem.endswith("_comp") else "solo")
        groups[g].append({"audio": str(audio), "group": g, "truth": ("gs", str(jams))})
    for line in open(data / "eval_egdb.tsv"):
        audio, _, g, *_ = line.rstrip("\n").split("\t")
        if g == "egdb/real":
            continue  # real clips: labels without strings in this layout
        clip = Path(audio).stem
        groups[g].append({"audio": audio, "group": g, "truth": ("egdb", str(egdb / "audio_label" / f"{clip}.midi"))})
    # The same EGDB clips over band backing (`make_mixes.py`), raw and as
    # separated guitar stems (`bench_separation.py`).
    for manifest in ("eval_mix.tsv", "eval_mix_htdemucs_6s.tsv", "eval_mix_bsrofo_sw.tsv"):
        if not (data / manifest).exists():
            continue
        for line in open(data / manifest):
            audio, notes, g, *_ = line.rstrip("\n").split("\t")
            if g.endswith("/egdb/Marshall"):
                clip = Path(notes).stem
                groups[g].append({"audio": audio, "group": g, "truth": ("egdb", str(egdb / "audio_label" / f"{clip}.midi"))})
    out = []
    for g in sorted(groups):
        for it in spread(groups[g], limit):
            it["id"] = re.sub(r"[^A-Za-z0-9_.-]", "_", f"{g}__{Path(it['audio']).stem}")
            out.append(it)
    return out


def truth(it):
    kind, path = it["truth"]
    return guitarset_tab(path) if kind == "gs" else egdb_tab(path)


# ---------------------------------------------------------------- fingering

def viterbi(binary, cases):
    """`cases`: list of note lists [(onset, pitch)]; returns [(string, fret)] lists."""
    lines = "\n".join(json.dumps({"tuning": STANDARD, "notes": [[float(t), int(p)] for t, p in c]}) for c in cases)
    res = subprocess.run([str(binary)], input=lines + "\n", capture_output=True, text=True, check=True)
    return [json.loads(l) for l in res.stdout.splitlines()]


def lowest(notes):
    """Lowest fret per note; notes starting within 100 ms take distinct strings."""
    out = [None] * len(notes)
    order = sorted(range(len(notes)), key=lambda i: notes[i][0])
    group, used = None, set()
    for i in order:
        t, p = notes[i]
        if group is None or t - group > 0.1:
            group, used = t, set()
        options = sorted(((p - o, s + 1) for s, o in enumerate(STANDARD) if 0 <= p - o <= 24 and s + 1 not in used))
        if options:
            fret, s = options[0]
            used.add(s)
            out[i] = (s, fret)
        else:
            out[i] = (0, -1)
    return out


# ---------------------------------------------------------------- TabCNN

class TabCNN:
    SR, HOP = 22050, 512

    def __init__(self, path, gpfx):
        import onnxruntime as ort

        self.sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
        self.gpfx = gpfx

    def notes(self, audio_path):
        import librosa

        y, _ = librosa.load(audio_path, sr=self.SR, mono=True)
        if not self.gpfx:
            y = librosa.util.normalize(y)
        c = np.abs(librosa.cqt(y, sr=self.SR, hop_length=self.HOP, n_bins=192, bins_per_octave=24))
        if self.gpfx:
            c = librosa.amplitude_to_db(c, ref=np.max)
            c = (c - c.min()) / (c.max() - c.min() + 1e-9)
        r = np.pad(c.T, ((4, 4), (0, 0)))  # [frames + 8, 192]
        n = c.shape[1]
        win = np.stack([r[i : i + 9].T for i in range(n)])[..., None].astype(np.float32)  # [n, 192, 9, 1]
        name = self.sess.get_inputs()[0].name
        cls = np.concatenate([self.sess.run(None, {name: win[b : b + 2048]})[0].argmax(-1) for b in range(0, n, 2048)])
        dt = self.HOP / self.SR
        out = []
        for s in range(6):
            col = cls[:, s]
            k = 0
            while k < n:
                if col[k] == 0:
                    k += 1
                    continue
                j = k
                while j + 1 < n and col[j + 1] == col[k]:
                    j += 1
                if j - k + 1 >= 2:  # >= ~46 ms
                    # Output string 0 is the low E (GuitarSet order).
                    out.append((k * dt, (j + 1) * dt, STANDARD[5 - s] + int(col[k]) - 1, 6 - s))
                k = j + 1
        return sorted(out)


# ---------------------------------------------------------------- metrics

def f1_match(ref, est, key):
    """mir_eval onset-only F1 of notes grouped by `key` (pitch, or pitch and string)."""
    import mir_eval

    tp = 0
    buckets = defaultdict(lambda: ([], []))
    for n in ref:
        buckets[key(n)][0].append(n)
    for n in est:
        buckets[key(n)][1].append(n)
    for r, e in buckets.values():
        if r and e:
            iv = lambda ns: np.array([[max(a, 0.0), max(b, max(a, 0.0) + 0.001)] for a, b, *_ in ns])  # noqa: E731
            tp += len(mir_eval.transcription.match_notes(iv(r), hz([x[2] for x in r]), iv(e), hz([x[2] for x in e]), onset_tolerance=0.05, offset_ratio=None))
    return 2 * tp / max(1, len(ref) + len(est))


def scores(ref, est):
    return {
        "f50": f1_match(ref, est, lambda n: 0),
        "tab_f1": f1_match(ref, est, lambda n: n[3]),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--guitarset", type=Path, required=True)
    ap.add_argument("--egdb", type=Path, required=True)
    ap.add_argument("--egset12", type=Path, default=None, help="EGSet12 folder (same JAMS layout as GuitarSet)")
    ap.add_argument("--bench", type=Path, required=True, help="bench_notes.py output (notes cache)")
    ap.add_argument("--notes-engines", default="crnn-gaps,crnn-v2_spark_s5000,basic-pitch,yourmt3")
    ap.add_argument("--fingering-bin", type=Path, required=True)
    ap.add_argument("--tabcnn", type=Path, required=True, help="folder with tabcnn.onnx and tabcnn-gpfx.onnx")
    ap.add_argument("--limit", type=int, default=12)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    for k in ("data", "guitarset", "egdb", "bench", "fingering_bin", "tabcnn"):
        setattr(args, k, getattr(args, k).expanduser())
    args.egset12 = args.egset12.expanduser() if args.egset12 else None
    items = sets(args.data, args.guitarset, args.egdb, args.limit, args.egset12)
    tab_models = {"tabcnn": TabCNN(args.tabcnn / "tabcnn.onnx", False), "tabcnn-gpfx": TabCNN(args.tabcnn / "tabcnn-gpfx.onnx", True)}
    rows = defaultdict(lambda: defaultdict(list))
    cache = (args.out or args.bench) / "tab_notes"
    cache.mkdir(parents=True, exist_ok=True)
    truths = [truth(it) for it in items]
    # Fingering from the true notes.
    vit = viterbi(args.fingering_bin, [[(n[0], n[2]) for n in t] for t in truths])
    for it, t, v in zip(items, truths, vit):
        low = lowest([(n[0], n[2]) for n in t])
        for name, placed in (("fingering: viterbi (true notes)", v), ("fingering: lowest fret (true notes)", low)):
            rows[name][it["group"]].append({"string_acc": float(np.mean([s == n[3] for (s, _), n in zip(placed, t)])) if t else 0.0})
    # Modular: cached transcriptions + Viterbi.
    for eng in args.notes_engines.split(","):
        ests, ok = [], []
        for it in items:
            f = args.bench / "notes" / eng / f"{it['id']}.json"
            if f.exists():
                ests.append([(n[0], n[1], n[2]) for n in json.load(open(f)) if len(n) < 4 or "guitar" in n[3]])
                ok.append(it)
        placed = viterbi(args.fingering_bin, [[(n[0], n[2]) for n in e] for e in ests])
        for it, e, pl in zip(ok, ests, placed):
            est = [(a, b, p, s) for (a, b, p), (s, _) in zip(e, pl) if s > 0]
            rows[f"modular: {eng} + viterbi"][it["group"]].append(scores(truths[items.index(it)], est))
        if len(ok) < len(items):
            print(f"{eng}: {len(items) - len(ok)} items without cached notes", file=sys.stderr)
    # End-to-end.
    for name, model in tab_models.items():
        for it, t in zip(items, truths):
            f = cache / name / f"{it['id']}.json"
            if f.exists():
                est = [tuple(n) for n in json.load(open(f))]
            else:
                est = model.notes(it["audio"])
                f.parent.mkdir(parents=True, exist_ok=True)
                json.dump(est, open(f, "w"))
            rows[f"end-to-end: {name}"][it["group"]].append(scores(t, est))
        print(f"{name} done", file=sys.stderr)
    groups = sorted({g for r in rows.values() for g in r})
    report = {m: {g: {k: float(np.mean([x[k] for x in v])) for k in v[0]} | {"n": len(v)} for g, v in r.items()} for m, r in rows.items()}
    json.dump(report, open((args.out or args.bench) / "tab_scores.json", "w"), indent=1)
    for metric in ("string_acc", "f50", "tab_f1"):
        methods = [m for m in report if any(metric in c for c in report[m].values())]
        print(f"\n## {metric}\n| method | " + " | ".join(groups) + " |\n|---|" + "---|" * len(groups))
        for m in methods:
            print(f"| {m} | " + " | ".join(f"{report[m][g][metric]:.3f}" if g in report[m] else "" for g in groups) + " |")


if __name__ == "__main__":
    main()
