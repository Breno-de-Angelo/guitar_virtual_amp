"""String accuracy on the true notes of the test split, per set and per
tone, for fingering checkpoints with and without audio (`train.py`'s
`model.pt` and `meta.json`). Every model is scored on the same notes: GOAT
uses the fine-aligned MIDI notes with the tab's strings (`aligned`, notes
without a matched string are context only), because the audio follows those,
so its numbers differ from `bench.py`'s (tab notes, score time). A model
without audio gives the same strings for every tone of a recording.

Usage:
  python eval_audio.py --data data.json --audio fingering_audio \\
      --model ship=runs/ship_s1/model.pt,runs/ship_s2/model.pt --model audio=runs/audio_s1/model.pt \\
      --out eval_audio.json
"""
import argparse
import json
from collections import defaultdict
from pathlib import Path

import numpy as np
import torch

from common import decode, sort_notes
from model import Tagger
from train import attach_audio, predict

AMPS = {"egdb": ("Marshall", "Mesa", "Plexi"), "goat": tuple(f"amp_{k}" for k in range(1, 6))}


def load_model(path, device):
    ck = torch.load(path, map_location=device)
    model = Tagger(**ck["cfg"]).to(device).eval()
    model.load_state_dict(ck["state"])
    meta = json.load(open(Path(path).with_name("meta.json")))
    return model, meta["decode"]


def test_recs(data, audio_dir):
    recs = [r for r in json.load(open(data))["recordings"] if r["split"] == "test"]
    for r in recs:
        if r["source"] == "goat":
            r["notes"] = r["aligned"]
        r["notes"] = [r["notes"][i] for i in sort_notes(r["notes"])]
    return attach_audio(recs, audio_dir)


def score(model, dec, recs, device):
    """{(group, tone): [accuracy per item]}."""
    out = defaultdict(list)
    for r in recs:
        truth = np.array([x[3] - 1 for x in r["notes"]])
        mask = truth >= 0
        if not mask.any():
            continue
        fixed = None
        for tone in r["tones"]:
            if model.audio is None:
                fixed = fixed or predict(model, r, device)
                lg, frets, cid = fixed
            else:
                lg, frets, cid = predict(model, r, device, tone=tone)
            s = decode(lg, frets, cid, **dec)
            out[(r["group"], tone[0])].append(float(np.mean(s[mask] == truth[mask])))
    return out


def summary(per):
    """Rows of the report: each set, split by tone where it has several."""
    rows = {}
    groups = sorted({g for g, _ in per})
    for g in groups:
        tones = sorted(t for gg, t in per if gg == g)
        rows[g if len(tones) == 1 else f"{g} (all tones)"] = float(np.mean([np.mean(per[(g, t)]) for t in tones]))
        if g in AMPS:
            rows[f"{g} DI"] = float(np.mean(per[(g, "DI" if g == "egdb" else "di")]))
            rows[f"{g} amps"] = float(np.mean([np.mean(per[(g, t)]) for t in AMPS[g] if (g, t) in per]))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--audio", type=Path, required=True)
    ap.add_argument("--model", action="append", required=True, help="name=ckpt.pt[,ckpt.pt...] (seeds)")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    device = "cuda" if torch.cuda.is_available() else "cpu"
    recs = test_recs(args.data, args.audio)
    print(f"{len(recs)} test recordings with audio", flush=True)
    results, table = {}, {}
    for spec in args.model:
        name, paths = spec.split("=", 1)
        seeds = []
        for p in paths.split(","):
            model, dec = load_model(p, device)
            per = score(model, dec, recs, device)
            seeds.append(summary(per))
            results.setdefault(name, []).append({"ckpt": p, "rows": seeds[-1], "per_tone": {f"{g}|{t}": v for (g, t), v in per.items()}})
            print(f"{name} {p}: " + " ".join(f"{k}={v:.3f}" for k, v in seeds[-1].items()), flush=True)
        table[name] = {k: (float(np.mean([s[k] for s in seeds])), float(np.std([s[k] for s in seeds]))) for k in seeds[0]}
    rows = list(next(iter(table.values())))
    print("\n| Set | " + " | ".join(table) + " |\n|---|" + "---|" * len(table))
    for k in rows:
        print(f"| {k} | " + " | ".join(f"{t[k][0]:.3f} ± {t[k][1]:.3f}" for t in table.values()) + " |")
    if args.out:
        json.dump({"table": table, "runs": results}, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
