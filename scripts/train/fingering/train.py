"""Trains the fingering tagger on `data.py`'s train split, selects the
checkpoint and the decoding costs on the val split (test recordings are never
read), and exports ONNX (`export.py`).

Augmentation, label-preserving: random crops of up to `--crop` notes;
transposition by k semitones with every fret moved by k (a capo / position
shift) when all labelled frets stay in 0..22; a whole-tuning shift (e.g. Eb
standard, frets unchanged); and simulated transcription noise: dropped
notes, extra notes (octave / fifth / semitone neighbours, repeats; no label),
onset jitter and duration changes.

With `--audio DIR` (`audio.py`'s output) the tagger also sees each note's
spectrogram patch, from one of the recording's tones picked at random per
crop. Pitch augmentations then keep the audio pitch: the tuning shift
retunes the labels but reads the patch at the recorded pitch (a patch is
pitch-relative, so this approximates the same passage on a retuned guitar),
and transposition is refused.

Usage:
  python train.py --data data.json --out runs/ship_s1 --variant ship --seed 1
  python train.py --data data.json --out runs/full_s1 --variant full --seed 1
  python train.py --data data.json --audio fingering_audio --out runs/audio_s1 --seed 1
"""
import argparse
import json
import math
import time
from collections import defaultdict
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F

from common import INVALID, decode, features, labels, sort_notes, windowed
from model import PATCH_F, PATCH_T, Tagger

SELECT_GROUPS = ("guitarset/comp", "guitarset/solo", "egdb", "goat")


def load(path, variant, val_player=None):
    """Train / val recordings. With `val_player` (e.g. "02"), GuitarSet's val
    is every recording of that player and its other players are all train:
    a cross-player check, as the test players are new players."""
    recs = json.load(open(path))["recordings"]
    keep = lambda r: variant == "full" or r["source"] != "goat"  # noqa: E731
    split = lambda r: r["split"]  # noqa: E731
    if val_player:
        def split(r):
            if r["split"] != "test" and r["source"].startswith("guitarset"):
                return "val" if r["id"].split("/")[1].startswith(val_player + "_") else "train"
            return r["split"]
    train = [r for r in recs if split(r) == "train" and keep(r)]
    val = [r for r in recs if split(r) == "val" and keep(r)]
    for r in train + val:
        r["notes"] = [r["notes"][i] for i in sort_notes(r["notes"])]
    return train, val


def attach_audio(recs, audio_dir):
    """Adds `tones` [(name, spectrogram, shift)] from `audio.py`'s index and
    returns the recordings that have audio."""
    index = json.load(open(audio_dir / "index.json"))
    out = []
    for r in recs:
        if r["id"] in index:
            r["tones"] = [(e["tone"], np.load(audio_dir / e["spec"], mmap_mode="r"), float(e["shift"])) for e in index[r["id"]]]
            out.append(r)
    return out


def note_patches(notes, tone):
    """uint8 [N, PATCH_F, PATCH_T] for notes [onset, offset, pitch, string,
    audio pitch]."""
    from audio import patches

    _, spec, shift = tone
    if not notes:
        return np.zeros((0, PATCH_F, PATCH_T), np.uint8)
    return patches(spec, [x[0] + shift for x in notes], [x[4] if len(x) > 4 else x[2] for x in notes])


def augment(rec, rng, crop, noise, transpose=7, sym=False):
    notes, tuning = rec["notes"], list(rec["tuning"])
    n = len(notes)
    if n > crop:
        s = rng.integers(0, n - crop + 1)
        notes = notes[s : s + crop]
    notes = [list(x[:4]) + [x[2]] for x in notes]  # 5th: the pitch heard in the audio
    # Transposition with frets moved by k (strings unchanged).
    if transpose > 0 and rng.random() < 0.5:
        fr = [p - tuning[s - 1] for _, _, p, s in notes if 1 <= s <= 6 and 0 <= p - tuning[s - 1] <= 24]
        if fr:
            lo, hi = max(-transpose, -min(fr)), min(transpose, 22 - max(fr))
            if sym:  # same range up and down: no drift of the fret distribution
                lo, hi = -min(-lo, hi), min(-lo, hi)
            if hi >= lo:
                k = int(rng.integers(lo, hi + 1))
                for x in notes:
                    x[2] += k
    # Whole-tuning shift: pitches and open strings move together.
    if rng.random() < 0.2:
        d = int(rng.integers(-2, 1))
        tuning = [o + d for o in tuning]
        for x in notes:
            x[2] += d
    if noise and rng.random() < 0.6:
        keep = rng.random(len(notes)) >= rng.uniform(0, 0.15)
        kept = [x for x, k in zip(notes, keep) if k] or notes[:1]
        extra = []
        for _ in range(rng.binomial(len(kept), rng.uniform(0, 0.12))):
            src = kept[rng.integers(len(kept))]
            step = int(rng.choice([12, -12, 7, -5, 1, -1, 0, 2, -2]))
            on = src[0] + (rng.uniform(-0.04, 0.04) if rng.random() < 0.7 else rng.uniform(0.05, 0.4))
            extra.append([on, on + rng.uniform(0.05, 0.5), src[2] + step, 0, src[4] + step])
        notes = kept + extra
        jitter = rng.normal(0, 0.01, len(notes))
        scale = np.exp(rng.normal(0, 0.5, len(notes)))
        for x, j, sc in zip(notes, jitter, scale):
            dur = max(0.02, (x[1] - x[0]) * sc)
            x[0] += j
            x[1] = x[0] + dur
        notes = [x for x in notes if 20 <= x[2] <= 100]
    notes = [notes[i] for i in sort_notes(notes)]
    frets, cont, _ = features(notes, tuning)
    return frets, cont, labels(notes, frets), notes


def batch(recs, weights, rng, size, crop, noise, device, transpose, sym, audio=False):
    picks = rng.choice(len(recs), size, p=weights)
    items = [augment(recs[i], rng, crop, noise, transpose, sym) for i in picks]
    t = max(len(f) for f, _, _, _ in items)
    frets = np.full((size, t, 6), INVALID, np.int64)
    cont = np.zeros((size, t, items[0][1].shape[1]), np.float32)
    y = np.full((size, t), -100, np.int64)
    pad = np.ones((size, t), bool)
    patch = np.zeros((size, t, PATCH_F, PATCH_T), np.uint8) if audio else None
    for b, (f, c, lab, notes) in enumerate(items):
        frets[b, : len(f)], cont[b, : len(f)], y[b, : len(f)], pad[b, : len(f)] = f, c, lab, False
        if audio:
            tones = recs[picks[b]]["tones"]
            patch[b, : len(f)] = note_patches(notes, tones[rng.integers(len(tones))])
    to = lambda a: torch.from_numpy(a).to(device)  # noqa: E731
    return to(frets), to(cont), (to(patch).float() if audio else None), to(y), to(pad)


def predict(model, rec, device, window=128, tone=None):
    """Logits of a recording (notes in `sort_notes` order); `tone` one of
    `rec["tones"]` for an audio model."""
    frets, cont, cid = features(rec["notes"], rec["tuning"])
    extra = (note_patches(rec["notes"], tone),) if tone is not None else ()

    def run(f, c, *p):
        with torch.no_grad():
            pt = torch.from_numpy(np.ascontiguousarray(p[0])).to(device).float() if p else None
            return model(torch.from_numpy(f).to(device), torch.from_numpy(c).to(device), pt).float().cpu().numpy()

    return windowed(run, frets, cont, window, window // 2, extra), frets, cid


def val_cache(model, val, device, audio, n_tones):
    """[(rec, prediction)], one entry per val recording, or per recording and
    tone (first, middle and last tone, up to `n_tones`) for an audio model."""
    out = []
    for r in val:
        if not audio:
            out.append((r, predict(model, r, device)))
            continue
        k = len(r["tones"])
        for i in sorted({0, k // 2, k - 1})[:n_tones]:
            out.append((r, predict(model, r, device, tone=r["tones"][i])))
    return out


def evaluate(cache, span_cost, move_cost, pos_cost=0.0):
    """Mean per-item string accuracy per group, from cached logits."""
    per = defaultdict(list)
    for rec, (lg, frets, cid) in cache:
        s = decode(lg, frets, cid, span_cost, move_cost, pos_cost)
        truth = np.array([x[3] - 1 for x in rec["notes"]])
        if len(truth):
            per[rec["group"]].append(float(np.mean(s == truth)))
    return {g: float(np.mean(v)) for g, v in per.items()}


def select_score(acc):
    vals = [acc[g] for g in SELECT_GROUPS if g in acc]
    return float(np.mean(vals))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--variant", choices=("ship", "full"), default="ship")
    ap.add_argument("--kind", choices=("transformer", "lstm"), default="transformer")
    ap.add_argument("--d", type=int, default=128)
    ap.add_argument("--layers", type=int, default=4)
    ap.add_argument("--heads", type=int, default=4)
    ap.add_argument("--ff", type=int, default=256)
    ap.add_argument("--dropout", type=float, default=0.1)
    ap.add_argument("--steps", type=int, default=6000)
    ap.add_argument("--batch", type=int, default=64)
    ap.add_argument("--crop", type=int, default=128)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--no-noise", action="store_true")
    ap.add_argument("--transpose", type=int, default=7, help="max transposition in semitones (0: none)")
    ap.add_argument("--transpose-sym", action="store_true", help="transpose as far down as up")
    ap.add_argument("--solo-weight", type=float, default=1.0, help="sampling weight of GuitarSet solo notes")
    ap.add_argument("--val-player", default=None, help="GuitarSet player held out as val (cross-player)")
    ap.add_argument("--decode", default=None, help="span,move,pos decoding costs instead of the val grid search")
    ap.add_argument("--fixed-steps", action="store_true", help="keep the last checkpoint instead of the best val one")
    ap.add_argument("--goat-weight", type=float, default=1.0, help="sampling weight of GOAT notes (full)")
    ap.add_argument("--audio", type=Path, default=None, help="audio.py output: condition on spectrogram patches")
    ap.add_argument("--audio-dim", type=int, default=64)
    ap.add_argument("--audio-ch", type=int, default=16)
    ap.add_argument("--val-tones", type=int, default=3, help="tones per val recording (audio)")
    ap.add_argument("--eval-every", type=int, default=500)
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(args.seed)
    rng = np.random.default_rng(args.seed)
    device = "cuda" if torch.cuda.is_available() else "cpu"
    train, val = load(args.data, args.variant, args.val_player)
    audio = args.audio is not None
    if audio:
        if args.transpose > 0:
            raise SystemExit("--audio needs --transpose 0 (a transposed note would not match its audio)")
        n_tr, n_val = len(train), len(val)
        train, val = attach_audio(train, args.audio), attach_audio(val, args.audio)
        print(f"with audio: train {len(train)}/{n_tr}, val {len(val)}/{n_val} recordings", flush=True)
    sw = {"goat": args.goat_weight, "guitarset_solo": args.solo_weight}
    w = np.array([len(r["notes"]) * sw.get(r["source"], 1.0) for r in train], float)
    w /= w.sum()
    print(f"train {len(train)} recordings / {sum(len(r['notes']) for r in train)} notes, val {len(val)} / {sum(len(r['notes']) for r in val)}", flush=True)
    cfg = {k: getattr(args, k) for k in ("kind", "d", "layers", "heads", "ff", "dropout")}
    if audio:
        cfg.update(audio=args.audio_dim, audio_ch=args.audio_ch)
    model = Tagger(**cfg).to(device)
    params = sum(p.numel() for p in model.parameters())
    print(f"{args.kind}: {params} parameters", flush=True)
    opt = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=0.01)
    warm = 300
    sched = torch.optim.lr_scheduler.LambdaLR(opt, lambda s: min(1, (s + 1) / warm) * 0.5 * (1 + math.cos(math.pi * min(1, s / args.steps))))
    best, log, t0 = -1.0, [], time.time()
    for step in range(1, args.steps + 1):
        model.train()
        frets, cont, patch, y, pad = batch(train, w, rng, args.batch, args.crop, not args.no_noise, device, args.transpose, args.transpose_sym, audio)
        logits = model(frets, cont, patch, pad=pad)
        loss = F.cross_entropy(logits.reshape(-1, 6), y.reshape(-1), ignore_index=-100)
        opt.zero_grad()
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        sched.step()
        if step % args.eval_every == 0 or step == args.steps:
            model.eval()
            cache = val_cache(model, val, device, audio, args.val_tones)
            acc = evaluate(cache, 0.5, 0.0)
            score = select_score(acc)
            log.append({"step": step, "loss": float(loss.detach()), "val": acc, "score": score})
            print(f"step {step} loss {float(loss):.4f} val {score:.4f} " + " ".join(f"{g}={a:.3f}" for g, a in sorted(acc.items())) + f" ({time.time() - t0:.0f} s)", flush=True)
            if score > best or args.fixed_steps:
                best = score
                torch.save({"cfg": cfg, "state": model.state_dict(), "step": step}, args.out / "model.pt")
    # Decoding costs on val, with the best checkpoint.
    ck = torch.load(args.out / "model.pt", map_location=device)
    model.load_state_dict(ck["state"])
    model.eval()
    cache = val_cache(model, val, device, audio, args.val_tones)
    grid = []
    if args.decode:  # fixed costs (e.g. chosen on cross-player runs)
        span, move, pos = (float(x) for x in args.decode.split(","))
        acc = evaluate(cache, span, move, pos)
        grid.append((select_score(acc), span, move, pos, acc))
    for span in (() if args.decode else (0.0, 0.5, 1.0)):
        for move in (0.0, 0.2, 0.5, 1.0, 2.0, 4.0):
            for pos in (0.0, 0.03, 0.1):
                acc = evaluate(cache, span, move, pos)
                grid.append((select_score(acc), span, move, pos, acc))
                print(f"decode span {span} move {move} pos {pos}: {grid[-1][0]:.4f} " + " ".join(f"{g}={a:.3f}" for g, a in sorted(acc.items())), flush=True)
    score, span, move, pos, acc = max(grid, key=lambda g: g[0])
    meta = {"cfg": cfg, "params": params, "best_step": ck["step"], "variant": args.variant, "seed": args.seed,
            "decode": {"span_cost": span, "move_cost": move, "pos_cost": pos}, "val": acc, "val_score": score, "log": log, "args": {k: str(v) for k, v in vars(args).items()}}
    json.dump(meta, open(args.out / "meta.json", "w"), indent=1)
    print(f"best step {ck['step']}, decode span {span} move {move} pos {pos}, val {score:.4f} " + " ".join(f"{g}={a:.3f}" for g, a in sorted(acc.items())), flush=True)
    from export import export

    export(args.out / "model.pt", args.out / "model.onnx", val[:8], args.audio)


if __name__ == "__main__":
    main()
