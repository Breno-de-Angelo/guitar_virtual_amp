"""Note-level benchmark of transcription models on the labelled eval sets
(`prepare_data.py` manifests plus GuitarSet players 03-05).

Sets: the manifests (`--manifests`), GuitarSet players 03-05 (`--guitarset`)
and EGSet12 (`--egset12`).

Engines:
- `crnn:NAME=CHECKPOINT`: the guitar CRNN (any `finetune_crnn.py` or GAPS
  checkpoint), run like guitar_core::transcribe::guitar_crnn: 10 s windows
  keeping all but 1 s at inner edges, notes read with the same rules and
  `CrnnParams::guitar()`, then the pipeline's `prune_weak(0.1, 0.0)`.
- `basic-pitch`: Basic Pitch (Bittner et al. 2022, Apache-2.0), ONNX model,
  with the app's guitar note settings.
- `yourmt3[:SPACE_DIR]`: YourMT3+ YPTF.MoE+Multi (noPS), multi-instrument,
  from a local copy of the `mimbres/YourMT3` Space (default ~/gva/yourmt3).
- `muscriptor:SIZE:MODE`: MuScriptor (Rouard et al. 2026; CC-BY-NC weights,
  gated on Hugging Face). SIZE is small/medium/large or a local
  .safetensors path. MODE: `free` (no instrument conditioning), `known` (the
  instruments known to be present: the three guitar classes, plus electric
  bass, drums and voice on the band mixes). Onset times get the model's own
  lag correction (`BeatGrid.with_onset_delay`) when a beat grid is found.

Metrics (`score`), mean over items of each group:
- F50: mir_eval note onset F1, 50 ms, pitch within 50 cents (the metric of
  MT3, MuScriptor, GAPS and TART's audio-to-MIDI stage);
- F50+off: same plus offset within max(50 ms, 20% of the note);
- oct: share of reference notes missed at their pitch but found an octave or
  two away (onset within 50 ms), split into estimate too low / too high.
MuScriptor and YourMT3+ scores use their guitar-class notes; `any` also reports all pitched
notes, to show guitar notes it attributed to other instruments.

Usage:
  python bench_notes.py run --data PREPARED --guitarset GUITARSET --out BENCH \\
      --engine crnn:gaps=guitar-gaps.pth [--limit 12]
  python bench_notes.py run ... --engine muscriptor:small:known
  python bench_notes.py score --data PREPARED --guitarset GUITARSET --out BENCH [--limit 12]
Notes are cached per engine and item under BENCH/notes, so runs resume and
scoring never reruns models. Wall time per item goes to BENCH/timing.
"""
import argparse
import json
import math
import re
import sys
import time
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))

SR = 16000
MANIFESTS = ["eval_egdb.tsv", "eval_techs.tsv", "eval_goat.tsv", "eval_mix.tsv"]
GUITARS = ["acoustic_guitar", "clean_electric_guitar", "distorted_electric_guitar"]
BACKING = ["electric_bass", "drums", "voice"]


# ---------------------------------------------------------------- data

def read_notes(path):
    out = []
    for line in open(path):
        v = line.split()
        if len(v) >= 3:
            out.append((float(v[0]), float(v[1]), int(v[2])))
    return out


def guitarset_notes(jams):
    doc = json.load(open(jams))
    notes = []
    for a in doc["annotations"]:
        if a["namespace"] == "note_midi":
            for n in a["data"]:
                notes.append((n["time"], n["time"] + n["duration"], int(round(n["value"]))))
    return sorted(notes)


def spread(items, limit):
    """`limit` items spread evenly over the list (all when limit <= 0)."""
    if limit <= 0 or len(items) <= limit:
        return items
    return [items[int(i * len(items) / limit)] for i in range(limit)]


def items(data, guitarset, limit, manifests=None, egset12=None):
    groups = defaultdict(list)
    for m in manifests or MANIFESTS:
        p = data / m
        if not p.exists():
            continue
        for line in open(p):
            audio, notes, group, *_ = line.rstrip("\n").split("\t")
            groups[group].append({"audio": audio, "notes": notes, "group": group})
    if guitarset:
        for jams in sorted((guitarset / "annotation").glob("0[345]_*.jams")):
            audio = guitarset / "audio" / (jams.stem + "_mic.wav")
            kind = "comp" if jams.stem.endswith("_comp") else "solo"
            if audio.exists():
                groups[f"guitarset/{kind}"].append({"audio": str(audio), "jams": str(jams), "group": f"guitarset/{kind}"})
    if egset12:
        # EGSet12 (Pedroza et al., DAFx 2024; CC BY 4.0): 12 real electric
        # guitar performances, miked amp, JAMS labels in GuitarSet's format.
        for jams in sorted(egset12.glob("*.jams")):
            groups["egset12"].append({"audio": str(jams.with_suffix(".wav")), "jams": str(jams), "group": "egset12"})
    out = []
    for group in sorted(groups):
        for it in spread(groups[group], limit):
            stem = Path(it["audio"]).stem
            it["id"] = re.sub(r"[^A-Za-z0-9_.-]", "_", f"{group}__{stem}")
            out.append(it)
    return out


def truth(it):
    return guitarset_notes(it["jams"]) if "jams" in it else read_notes(it["notes"])


def load_audio(path, cache):
    from finetune_crnn import load_16k

    return np.asarray(load_16k(path, cache), np.float32) / 32768.0


# ---------------------------------------------------------------- CRNN

FPS = 100
SEGMENT = 10 * SR
KEEP = 1000
MARGIN = 100
LOWEST = 21
ONSET_T, OFFSET_T, FRAME_T, MIN_DUR, MIN_P, MAX_P = 0.3, 0.3, 0.1, 0.03, 28, 92


class Crnn:
    def __init__(self, checkpoint, device):
        import torch
        from export_guitar_crnn import GuitarCRNN

        self.torch, self.device = torch, device
        model = GuitarCRNN()
        state = torch.load(checkpoint, map_location="cpu", weights_only=False)
        state = state.get("model", state)
        model.load_state_dict({k: v for k, v in state.items() if not k.endswith("num_batches_tracked")})
        self.model = model.to(device).eval()

    def posteriors(self, audio):
        torch = self.torch
        hop = (KEEP - 2 * MARGIN) * (SR // FPS)
        segs = 1 if len(audio) <= SEGMENT else math.ceil((len(audio) - SEGMENT) / hop) + 1
        x = np.zeros((segs, SEGMENT), np.float32)
        for i in range(segs):
            chunk = audio[i * hop : i * hop + SEGMENT]
            x[i, : len(chunk)] = chunk
        outs = []
        with torch.no_grad():
            for b in range(0, segs, 8):
                o = self.model(torch.from_numpy(x[b : b + 8]).to(self.device))
                outs.append(np.stack([t.float().cpu().numpy() for t in o], 0))  # [4, B, 1001, 88]
        outs = np.concatenate(outs, 1)
        parts = []
        for i in range(segs):
            lo = 0 if i == 0 else MARGIN
            hi = KEEP if i == segs - 1 else KEEP - MARGIN
            parts.append(outs[:, i, lo:hi])
        post = np.concatenate(parts, 1)
        total = math.ceil(len(audio) / SR * FPS) + 1
        return post[:, :total]  # onset, offset, frame, velocity: [frames, 88]

    @staticmethod
    def peaks(x, threshold, nb):
        """Kong et al. regression peaks: above threshold, monotonic over `nb`
        frames each side; (frame, sub-frame shift)."""
        n = len(x)
        out = []
        if n <= 2 * nb:
            return out
        for i in np.nonzero(x > threshold)[0]:
            if i < nb or i >= n - nb:
                continue
            if all(x[i - k] >= x[i - k - 1] and x[i + k] >= x[i + k + 1] for k in range(nb)):
                prev, cur, nxt = float(x[i - 1]), float(x[i]), float(x[i + 1])
                denom = cur - nxt if prev > nxt else cur - prev
                shift = (nxt - prev) / denom / 2 if abs(denom) > 1e-9 else 0.0
                out.append((int(i), max(-0.5, min(0.5, shift))))
        return out

    def notes(self, audio):
        onset, offset, frame, velocity = self.posteriors(audio)
        n = onset.shape[0]
        notes = []
        for p in range(MIN_P - LOWEST, min(MAX_P - LOWEST, 87) + 1):
            ons = self.peaks(onset[:, p], ONSET_T, 2)
            if not ons:
                continue
            is_on = dict(ons)
            is_off = dict(self.peaks(offset[:, p], OFFSET_T, 4))

            def push(bgn, s, fin, fs):
                start, end = (bgn + s) / FPS, (fin + fs) / FPS
                if end - start >= MIN_DUR:
                    act = float(frame[bgn : min(fin, n - 1) + 1, p].mean())
                    amp = min(1.0, max(0.0, float(velocity[bgn, p]) * 0.5 + act * 0.5))
                    notes.append((start, end, p + LOWEST, amp))

            cur = gone = off_at = None
            for i in range(n):
                if i in is_on:
                    if cur is not None:
                        push(cur[0], cur[1], max(0, i - 1), 0.0)
                    cur, gone, off_at = (i, is_on[i]), None, None
                    continue
                if cur is None:
                    continue
                bgn, s = cur
                if frame[i, p] <= FRAME_T and gone is None:
                    gone = i
                if i in is_off and off_at is None:
                    off_at = (i, is_off[i])
                if gone is not None:
                    if off_at is not None and off_at[0] - bgn > gone - off_at[0]:
                        fin, fs = off_at
                    else:
                        fin, fs = gone, is_off.get(gone, 0.0)
                    push(bgn, s, fin, fs)
                    cur = gone = off_at = None
                elif i - bgn >= 600 or i == n - 1:
                    push(bgn, s, i, is_off.get(i, 0.0))
                    cur = off_at = None
        notes.sort()
        # prune_weak(ratio 0.1, overtone_ratio 0): drop notes far weaker than
        # the loudest one starting with them.
        kept = []
        for a in notes:
            loudest = max(b[3] for b in notes if abs(b[0] - a[0]) < 0.04)
            if a[3] >= loudest * 0.1:
                kept.append(a)
        return [[s, e, p, "guitar"] for s, e, p, _ in kept]


# ---------------------------------------------------------------- MuScriptor

class MuScriptor:
    def __init__(self, size, mode, device, dtype):
        from muscriptor import TranscriptionModel

        self.model = TranscriptionModel.load_model(size, device=device, dtype=dtype)
        self.mode = mode

    def notes(self, audio, group):
        import torch
        from muscriptor.events import NoteEndEvent

        wav = (torch.from_numpy(audio), SR)
        instruments = None
        if self.mode == "known":
            instruments = GUITARS + (BACKING if group.startswith("mix/") else [])
        events = list(self.model.transcribe(wav, instruments=instruments))
        delay = 0.0
        starts = [e.start_event.start_time for e in events if isinstance(e, NoteEndEvent)]
        try:
            grid = self.model.detect_beat_grid_for(wav)
            if grid is not None and starts:
                delay = grid.with_onset_delay(starts).onset_delay or 0.0
        except Exception as e:  # tempo detection is best effort
            print(f"  (no onset-delay correction: {e})", file=sys.stderr)
        out = []
        for e in events:
            if isinstance(e, NoteEndEvent):
                s = e.start_event
                if s.instrument != "drums":
                    out.append([s.start_time - delay, e.end_time - delay, s.pitch, s.instrument])
        return out, delay


# ---------------------------------------------------------------- Basic Pitch

class BasicPitch:
    """Spotify's Basic Pitch (ICASSP 2022) through its ONNX model, with the
    app's guitar settings (`NoteParams::guitar()`: onset 0.5, frame 0.3,
    notes of 5+ frames, MIDI 30-90)."""

    def __init__(self):
        from basic_pitch import ICASSP_2022_MODEL_PATH
        from basic_pitch.inference import Model

        self.model = Model(ICASSP_2022_MODEL_PATH)

    def notes(self, audio):
        import tempfile

        import soundfile as sf
        from basic_pitch.inference import predict

        with tempfile.NamedTemporaryFile(suffix=".wav") as f:
            sf.write(f.name, audio, SR)
            _, _, events = predict(f.name, self.model, onset_threshold=0.5, frame_threshold=0.3,
                                   minimum_note_length=58, minimum_frequency=46.2, maximum_frequency=1480.0,
                                   melodia_trick=True)
        return sorted([float(s), float(e), int(p), "guitar"] for s, e, p, *_ in events)


# ---------------------------------------------------------------- YourMT3+

YMT3_CKPT = "mc13_256_g4_all_v7_mt3f_sqr_rms_moe_wf4_n8k2_silu_rope_rp_b36_nops@last.ckpt"
YMT3_ARGS = ["-p", "2024", "-tk", "mc13_full_plus_256", "-dec", "multi-t5", "-nl", "26", "-enc", "perceiver-tf",
             "-sqr", "1", "-ff", "moe", "-wf", "4", "-nmoe", "8", "-kmoe", "2", "-act", "silu", "-epe", "rope",
             "-rp", "1", "-ac", "spec", "-hop", "300", "-atc", "1", "-pr", "16"]


class YourMT3:
    """YourMT3+ YPTF.MoE+Multi (noPS) (Chang et al. 2024; weights Apache-2.0,
    code from the `mimbres/YourMT3` Hugging Face Space at `root`, run as in
    its `model_helper.transcribe`). Notes of MIDI programs 24-31 are guitar."""

    def __init__(self, root, device):
        import os

        root = Path(root).expanduser()
        sys.path[:0] = [str(root / "amt" / "src"), str(root)]
        cwd = os.getcwd()
        os.chdir(root)  # checkpoint paths are relative to the Space root
        try:
            from model_helper import load_model_checkpoint

            self.model = load_model_checkpoint(args=[YMT3_CKPT] + YMT3_ARGS, device="cpu").to(device)
        finally:
            os.chdir(cwd)
        self.device = device

    def notes(self, audio):
        import torch
        from utils.audio import slice_padded_array
        from utils.event2note import merge_zipped_note_events_and_ties_to_notes
        from utils.note2event import mix_notes

        m = self.model
        frames, rate = m.audio_cfg["input_frames"], m.audio_cfg["sample_rate"]
        assert rate == SR
        segs = slice_padded_array(audio[None].astype(np.float32), frames, frames)
        segs = torch.from_numpy(segs.astype("float32")).to(self.device).unsqueeze(1)
        with torch.no_grad():
            tokens, _ = m.inference_file(bsz=8, audio_segments=segs)
        starts = [frames * i / rate for i in range(segs.shape[0])]
        per_channel = []
        for ch in range(m.task_manager.num_decoding_channels):
            zipped, _, _ = m.task_manager.detokenize_list_batches([a[:, ch, :] for a in tokens], starts, return_events=True)
            per_channel.append(merge_zipped_note_events_and_ties_to_notes(zipped)[0])
        out = []
        for n in mix_notes(per_channel):
            if not n.is_drum:
                inst = "guitar" if 24 <= n.program <= 31 else f"program{n.program}"
                out.append([float(n.onset), float(n.offset), int(n.pitch), inst])
        return sorted(out)


# ---------------------------------------------------------------- run

def engine_name(spec):
    kind, rest = (spec.split(":", 1) + [""])[:2]
    if kind == "crnn":
        return "crnn-" + rest.split("=", 1)[0]
    if kind in ("basic-pitch", "yourmt3"):
        return kind
    size, mode = rest.split(":")[:2]
    return f"muscriptor-{Path(size).stem}-{mode}"


def run(args):
    name = engine_name(args.engine)
    kind, rest = (args.engine.split(":", 1) + [""])[:2]
    if kind == "basic-pitch":
        engine = BasicPitch()
    elif kind == "yourmt3":
        engine = YourMT3(rest or "~/gva/yourmt3", args.device)
    elif kind == "crnn":
        engine = Crnn(Path(rest.split("=", 1)[1]).expanduser(), args.device)
    elif kind == "muscriptor":
        size, mode = rest.split(":")[:2]
        assert mode in ("free", "known"), mode
        engine = MuScriptor(size, mode, args.device, args.dtype)
    else:
        raise SystemExit(f"unknown engine {kind}")
    notes_dir = args.out / "notes" / name
    notes_dir.mkdir(parents=True, exist_ok=True)
    (args.out / "timing").mkdir(exist_ok=True)
    timing = open(args.out / "timing" / f"{name}.jsonl", "a")
    todo = [it for it in items(args.data, args.guitarset, args.limit, args.manifests, args.egset12) if not (notes_dir / f"{it['id']}.json").exists()]
    print(f"{name}: {len(todo)} items to transcribe", flush=True)
    cache = args.data / "cache16k"
    for k, it in enumerate(todo):
        audio = load_audio(it["audio"], cache)
        t0 = time.time()
        if kind in ("crnn", "basic-pitch", "yourmt3"):
            notes, extra = engine.notes(audio), {}
        else:
            notes, delay = engine.notes(audio, it["group"])
            extra = {"onset_delay": delay}
        dt = time.time() - t0
        json.dump(notes, open(notes_dir / f"{it['id']}.json", "w"))
        timing.write(json.dumps({"id": it["id"], "audio_s": len(audio) / SR, "wall_s": dt, **extra}) + "\n")
        timing.flush()
        print(f"[{k + 1}/{len(todo)}] {it['id']} {len(audio) / SR:.0f}s audio in {dt:.1f}s, {len(notes)} notes", flush=True)


# ---------------------------------------------------------------- score

def hz(p):
    return 440.0 * 2 ** ((np.asarray(p, float) - 69) / 12)


def intervals(notes):
    iv = np.array([[s, max(e, s + 0.001)] for s, e, *_ in notes], float).reshape(-1, 2)
    return iv, hz([n[2] for n in notes])


def metrics(ref, est):
    import mir_eval

    if not ref:
        return None
    ri, rp = intervals(ref)
    if not est:
        return {"f50": 0.0, "f50_off": 0.0, "oct_low": 0.0, "oct_high": 0.0}
    ei, ep = intervals(est)
    tr = mir_eval.transcription
    f50 = tr.precision_recall_f1_overlap(ri, rp, ei, ep, onset_tolerance=0.05, offset_ratio=None)[2]
    f50_off = tr.precision_recall_f1_overlap(ri, rp, ei, ep, onset_tolerance=0.05, offset_ratio=0.2)[2]
    matched = {r for r, _ in tr.match_notes(ri, rp, ei, ep, onset_tolerance=0.05, offset_ratio=None)}
    est_on = np.array([n[0] for n in est])
    est_p = np.array([n[2] for n in est])
    low = high = 0
    for r, (s, _, p) in enumerate(ref):
        if r in matched:
            continue
        near = est_p[np.abs(est_on - s) <= 0.05] - p
        if np.isin(near, [-12, -24]).any():
            low += 1
        elif np.isin(near, [12, 24]).any():
            high += 1
    return {"f50": f50, "f50_off": f50_off, "oct_low": low / len(ref), "oct_high": high / len(ref)}


def score(args):
    all_items = items(args.data, args.guitarset, args.limit, args.manifests, args.egset12)
    engines = sorted(p.name for p in (args.out / "notes").iterdir() if p.is_dir())
    rows = defaultdict(lambda: defaultdict(list))  # (engine, variant) -> group -> metrics
    for it in all_items:
        ref = truth(it)
        for eng in engines:
            f = args.out / "notes" / eng / f"{it['id']}.json"
            if not f.exists():
                continue
            est = json.load(open(f))
            variants = {"guitar": [n for n in est if "guitar" in n[3]]}
            if eng.startswith(("muscriptor", "yourmt3")):
                variants["any"] = est
            for v, notes in variants.items():
                m = metrics(ref, notes)
                if m:
                    rows[(eng, v)][it["group"]].append(m)
    groups = sorted({g for r in rows.values() for g in r})
    report = {}
    for key in sorted(rows):
        label = key[0] if key[1] == "guitar" else f"{key[0]} (any)"
        report[label] = {}
        for g in groups:
            ms = rows[key].get(g)
            if ms:
                report[label][g] = {k: float(np.mean([m[k] for m in ms])) for k in ms[0]} | {"n": len(ms)}
    json.dump(report, open(args.out / "scores.json", "w"), indent=1)
    for metric in ("f50", "f50_off", "oct_low"):
        print(f"\n## {metric} (mean per item)")
        print("| group | " + " | ".join(report) + " |")
        print("|---|" + "---|" * len(report))
        for g in groups:
            cells = []
            for label in report:
                c = report[label].get(g)
                cells.append(f"{c[metric]:.3f} ({c['n']})" if c else "")
            print(f"| {g} | " + " | ".join(cells) + " |")
    print("\n## wall time (audio s per wall s)")
    for f in sorted((args.out / "timing").glob("*.jsonl")):
        rs = [json.loads(l) for l in open(f)]
        a, w = sum(r["audio_s"] for r in rs), sum(r["wall_s"] for r in rs)
        print(f"{f.stem}: {a / max(w, 1e-9):.2f}x real time over {a / 60:.0f} min of audio")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["run", "score"])
    ap.add_argument("--data", type=Path, required=True, help="prepare_data.py output (eval_*.tsv, cache16k)")
    ap.add_argument("--guitarset", type=Path, default=None, help="GuitarSet folder (annotation/, audio/)")
    ap.add_argument("--egset12", type=Path, default=None, help="EGSet12 folder (NN.wav, NN.jams; Zenodo 11406378)")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--engine", default=None)
    ap.add_argument("--manifests", default=None, help="comma-separated manifest files in --data (default: " + ",".join(MANIFESTS) + ")")
    ap.add_argument("--limit", type=int, default=12, help="items per group, spread evenly (0 = all)")
    ap.add_argument("--device", default="cuda")
    ap.add_argument("--dtype", default=None, help="MuScriptor transformer dtype (float32, bfloat16)")
    args = ap.parse_args()
    args.data, args.out = args.data.expanduser(), args.out.expanduser()
    args.manifests = args.manifests.split(",") if args.manifests else None
    args.guitarset = args.guitarset.expanduser() if args.guitarset else None
    args.egset12 = args.egset12.expanduser() if args.egset12 else None
    args.out.mkdir(parents=True, exist_ok=True)
    if args.cmd == "run":
        if not args.engine:
            raise SystemExit("--engine is required for run")
        run(args)
    else:
        score(args)


if __name__ == "__main__":
    main()
