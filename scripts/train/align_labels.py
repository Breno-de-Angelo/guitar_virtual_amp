"""Removes constant label latency per recording: hexaphonic MIDI pickups
(Guitar-TECHS) report notes 25-40 ms late or early depending on the take,
and IDMT marks onsets ~27 ms before the attack. For each notes file, the
original GAPS model's onsets on the recording are matched to the labels
(same pitch, within 150 ms) and all label times are shifted by the median
offset, when at least 20 notes match and the offset is under 100 ms.
Recordings with fewer matches take the median shift of their folder (one
take or annotation convention), when 3 or more of its files were measured.
Only a constant shift per recording; note-level timing is untouched.

Rewrites the notes files in place (keeping `<file>.orig`) and logs the
shifts to `<data>/label_shifts.tsv`. Applies to the `techs` and `goat` training
recordings and the eval manifests given with --manifest. `--device cpu`
when the GPU is busy.

Usage: python align_labels.py --data ~/.cache/gva_test/prepared --init guitar-gaps.pth \\
         --manifest eval_techs.tsv --manifest eval_techs_techniques.tsv --manifest eval_idmt.tsv
"""

import argparse
import json
import shutil
import sys
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
from scipy.signal import resample_poly

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from export_guitar_crnn import SAMPLE_RATE, SEGMENT, GuitarCRNN  # noqa: E402


DEVICE = "cuda" if torch.cuda.is_available() else "cpu"


def onsets(model, x):
    out = []
    for s in range(0, len(x), SEGMENT):
        seg = np.zeros(SEGMENT, np.float32)
        chunk = x[s : s + SEGMENT]
        seg[: len(chunk)] = chunk
        with torch.no_grad():
            on = model(torch.from_numpy(seg)[None].to(DEVICE))[0][0].cpu().numpy()
        peaks = (on > 0.3) & (on >= np.roll(on, 1, 0)) & (on > np.roll(on, -1, 0))
        out += [(t / 100 + s / SAMPLE_RATE, p + 21) for t, p in zip(*np.nonzero(peaks))]
    return out


def shift_for(model, audio, notes_path):
    x, sr = sf.read(audio, dtype="float32", always_2d=True)
    x = x.mean(axis=1)
    g = np.gcd(sr, SAMPLE_RATE)
    x = resample_poly(x, SAMPLE_RATE // g, sr // g).astype(np.float32)[: SAMPLE_RATE * 120]
    est = onsets(model, x)
    offs = []
    for line in open(notes_path):
        v = line.split()
        t, p = float(v[0]), int(v[2])
        if t > 120:
            break
        c = [e[0] - t for e in est if e[1] == p and abs(e[0] - t) < 0.15]
        if c:
            offs.append(min(c, key=abs))
    if len(offs) < 20:
        return None, len(offs)
    shift = float(np.median(offs))
    return (shift if abs(shift) < 0.1 else None), len(offs)


def rewrite(notes_path, shift):
    orig = Path(str(notes_path) + ".orig")
    if not orig.exists():
        shutil.copy(notes_path, orig)
    lines = []
    for line in open(orig):
        v = line.split()
        v[0] = f"{max(0.0, float(v[0]) + shift):.4f}"
        v[1] = f"{max(0.0, float(v[1]) + shift):.4f}"
        lines.append(" ".join(v) + "\n")
    open(notes_path, "w").writelines(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--init", type=Path, required=True)
    ap.add_argument("--manifest", action="append", default=[])
    ap.add_argument("--device", default=DEVICE)
    args = ap.parse_args()
    globals()["DEVICE"] = args.device
    data = args.data.expanduser()
    model = GuitarCRNN()
    state = torch.load(args.init.expanduser(), map_location="cpu", weights_only=False)["model"]
    model.load_state_dict({k: v for k, v in state.items() if not k.endswith("num_batches_tracked")})
    model = model.eval().to(DEVICE)
    jobs = {}
    for line in open(data / "train.jsonl"):
        item = json.loads(line)
        if item["source"].startswith(("techs", "goat")):
            di = next((a for a in item["audio"] if "directinput" in a), item["audio"][0])
            jobs.setdefault(item["notes"], di)
    for m in args.manifest:
        for line in open(data / m):
            audio, notes, *_ = line.rstrip("\n").split("\t")
            jobs.setdefault(notes, audio)
    measured = {}
    for notes, audio in jobs.items():
        orig = Path(notes + ".orig")
        src = orig if orig.exists() else Path(notes)
        measured[notes] = shift_for(model, audio, src)
    by_folder = {}
    for notes, (shift, _) in measured.items():
        if shift is not None:
            by_folder.setdefault(str(Path(notes).parent), []).append(shift)
    with open(data / "label_shifts.tsv", "w") as log:
        for notes, (shift, n) in measured.items():
            folder = by_folder.get(str(Path(notes).parent), [])
            if shift is None and len(folder) >= 3:
                shift = float(np.median(folder))
            orig = Path(notes + ".orig")
            if orig.exists():
                shutil.copy(orig, notes)
            if shift is not None:
                rewrite(Path(notes), shift)
            shown = "" if shift is None else round(shift * 1000, 1)
            log.write(f"{notes}\t{jobs[notes]}\t{n}\t{shown}\n")
            print(f"{Path(notes).name}: {n} matches, shift {shown} ms", flush=True)


if __name__ == "__main__":
    main()
