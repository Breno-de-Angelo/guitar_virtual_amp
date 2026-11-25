"""Fine-tunes the guitar CRNN (Kong et al. 2021 architecture, GAPS weights of
Riley et al. 2024) on electric guitar in many tones, then exports it to ONNX
for guitar_core::transcribe::guitar_crnn (`GVA_GUITAR_MODEL=<file>`).

Training data comes from `prepare_data.py` (`train.jsonl`): every recording
lists several variants of the same performance (DI, miked amp, amp renders,
`render_tones` presets); each training example picks one at random, so the
network sees the same notes in many tones (as in EGDB-PG, 2025, and Pedroza
et al., 2024).

Targets follow Kong et al.: frame activity, onset and offset regression
(1 - |distance| / 5 frames around each event, at 100 frames per second) and
velocity at the onset frame. BatchNorm statistics stay frozen (small
batches), and the learning rate is low, to keep what the model learnt on
classical guitar.

Usage:
  python finetune_crnn.py --data ~/.cache/gva_test/prepared --init guitar-gaps.pth \\
      --out runs/electric [--steps 8000] [--batch 4] [--accum 2] [--seconds 4] [--lr 1e-4]
Writes `<out>/model.pt` (best on the held-out EGDB tabs), `<out>/model.onnx`
and `<out>/log.jsonl`.
"""

import argparse
import hashlib
import json
import math
import random
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
import torch.nn as nn
import torch.nn.functional as F
from scipy.signal import resample_poly

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from export_guitar_crnn import PITCHES, SAMPLE_RATE, SEGMENT, GuitarCRNN  # noqa: E402

FPS = 100
FRAMES = SEGMENT // (SAMPLE_RATE // FPS) + 1  # 1001
LOWEST = 21
J = 5  # regression half-width in frames


def load_16k(path, cache):
    """Mono int16 audio at 16 kHz, cached as .npy next to the prepared data."""
    key = cache / (hashlib.sha1(str(path).encode()).hexdigest() + ".npy")
    if key.exists():
        return np.load(key, mmap_mode="r")
    x, sr = sf.read(path, dtype="float32", always_2d=True)
    x = x.mean(axis=1)
    if sr != SAMPLE_RATE:
        g = math.gcd(sr, SAMPLE_RATE)
        x = resample_poly(x, SAMPLE_RATE // g, sr // g).astype(np.float32)
    peak = max(1e-6, float(np.abs(x).max()))
    x = np.clip(x / peak * 0.9 * 32767, -32767, 32767).astype(np.int16)
    np.save(key, x)
    return np.load(key, mmap_mode="r")


def read_notes(path):
    notes = []
    for line in open(path):
        v = line.split()
        if len(v) >= 3:
            notes.append((float(v[0]), float(v[1]), int(v[2]), int(v[3]) if len(v) > 3 else 0))
    return notes


def targets(notes, start, frames=FRAMES):
    """Kong-style rolls for `frames` frames from `start` seconds."""
    FRAMES = frames  # noqa: N806 (shadows the 10 s default)
    frame = np.zeros((FRAMES, PITCHES), np.float32)
    onset = np.zeros_like(frame)
    offset = np.zeros_like(frame)
    velocity = np.zeros_like(frame)
    vel_mask = np.zeros_like(frame)
    idx = np.arange(FRAMES, dtype=np.float32)
    end = start + (FRAMES - 1) / FPS
    for on, off, pitch, vel in notes:
        p = pitch - LOWEST
        if not (0 <= p < PITCHES) or off < start or on > end:
            continue
        a, b = (on - start) * FPS, (off - start) * FPS
        lo, hi = max(0, int(math.floor(a))), min(FRAMES - 1, int(math.ceil(b)))
        frame[lo : hi + 1, p] = 1.0
        for t, roll in ((a, onset), (b, offset)):
            if -J <= t <= FRAMES - 1 + J:
                roll[:, p] = np.maximum(roll[:, p], np.clip(1.0 - np.abs(idx - t) / J, 0.0, 1.0))
        k = int(round(a))
        if 0 <= k < FRAMES and vel > 0:
            velocity[k, p] = vel / 128.0
            vel_mask[k, p] = 1.0
    return frame, onset, offset, velocity, vel_mask


class Clips(torch.utils.data.Dataset):
    """Random 10 s crops of random variants, with gain augmentation."""

    def __init__(self, items, cache, length, seconds):
        self.items, self.cache, self.length = items, cache, length
        self.samples = int(seconds * SAMPLE_RATE)
        self.frames = self.samples // (SAMPLE_RATE // FPS) + 1

    def __len__(self):
        return self.length

    def __getitem__(self, i):
        rng = random.Random()
        item = rng.choice(self.items)
        audio = load_16k(rng.choice(item["audio"]), self.cache)
        notes = item["notes_list"]
        n, seg = len(audio), self.samples
        if n > seg:
            # Start where something is played most of the time.
            first = max(0.0, min(on for on, *_ in notes) - 1.0) if notes else 0.0
            lo = min(int(first * SAMPLE_RATE), n - seg)
            s = rng.randint(lo, n - seg)
        else:
            s = 0
        x = np.zeros(seg, np.float32)
        chunk = np.asarray(audio[s : s + seg], np.float32) / 32768.0
        x[: len(chunk)] = chunk
        x *= 10 ** (rng.uniform(-18, 0) / 20)
        if rng.random() < 0.3:
            x += np.random.randn(seg).astype(np.float32) * 10 ** (rng.uniform(-70, -45) / 20)
        rolls = targets(notes, s / SAMPLE_RATE, self.frames)
        return (torch.from_numpy(x),) + tuple(torch.from_numpy(r) for r in rolls)


def checkpoint_acoustic_models(model):
    """Recomputes the conv stacks in the backward pass instead of keeping
    their activations (they don't fit in 8 GB otherwise)."""
    from torch.utils.checkpoint import checkpoint

    for name in ("frame_model", "reg_onset_model", "reg_offset_model", "velocity_model"):
        sub = getattr(model, name)
        plain = sub.forward
        sub.forward = lambda x, plain=plain: (
            checkpoint(plain, x, use_reentrant=False) if torch.is_grad_enabled() else plain(x)
        )


def freeze_bn(model):
    for m in model.modules():
        if isinstance(m, (nn.BatchNorm1d, nn.BatchNorm2d)):
            m.eval()
            for p in m.parameters():
                p.requires_grad_(False)


def loss_fn(out, batch):
    onset, offset, frame, velocity = out
    _, f_t, on_t, off_t, vel_t, vel_m = batch
    bce = F.binary_cross_entropy
    l_vel = (F.binary_cross_entropy(velocity, vel_t, reduction="none") * vel_m).sum() / vel_m.sum().clamp(min=1)
    return bce(onset, on_t) + bce(offset, off_t) + bce(frame, f_t) + l_vel


@torch.no_grad()
def validate(model, items, cache, device):
    """Onset F1 (peaks of the onset curve above 0.3, same pitch, 50 ms) over
    10 s windows of the held-out recordings."""
    model.eval()
    tp = fp = fn = 0
    for item in items:
        audio = np.asarray(load_16k(item["audio"], cache), np.float32) / 32768.0
        notes = item["notes_list"]
        for s in range(0, max(1, len(audio) - SAMPLE_RATE), SEGMENT):
            x = np.zeros(SEGMENT, np.float32)
            chunk = audio[s : s + SEGMENT]
            x[: len(chunk)] = chunk
            onset = model(torch.from_numpy(x)[None].to(device))[0][0].float().cpu().numpy()
            peaks = (onset > 0.3) & (onset >= np.roll(onset, 1, 0)) & (onset > np.roll(onset, -1, 0))
            est = [(t / FPS + s / SAMPLE_RATE, p + LOWEST) for t, p in zip(*np.nonzero(peaks))]
            t0, t1 = s / SAMPLE_RATE, (s + len(chunk)) / SAMPLE_RATE
            truth = [(on, p) for on, _, p, _ in notes if t0 <= on < t1]
            used = [False] * len(est)
            for on, p in truth:
                j = next((j for j, e in enumerate(est) if not used[j] and e[1] == p and abs(e[0] - on) <= 0.05), None)
                if j is None:
                    fn += 1
                else:
                    used[j] = True
                    tp += 1
            fp += used.count(False)
    model.train()
    freeze_bn(model)
    return 2 * tp / max(1, 2 * tp + fp + fn)


def export(model, path):
    model = model.float().cpu().eval()
    torch.onnx.export(
        model,
        (torch.zeros(1, SEGMENT),),
        str(path),
        input_names=["audio"],
        output_names=["onset", "offset", "frame", "velocity"],
        opset_version=17,
        dynamo=False,
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--init", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--steps", type=int, default=8000)
    ap.add_argument("--batch", type=int, default=4)
    ap.add_argument("--seconds", type=float, default=4.0, help="training crop length")
    ap.add_argument("--accum", type=int, default=2, help="batches per optimizer step")
    ap.add_argument("--checkpoint", action="store_true", help="gradient checkpointing (less memory, slower)")
    ap.add_argument("--lr", type=float, default=1e-4)
    ap.add_argument("--val-every", type=int, default=1000)
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--sources", default="", help="comma-separated source prefixes to train on (default: all)")
    args = ap.parse_args()
    data, out = args.data.expanduser(), args.out.expanduser()
    out.mkdir(parents=True, exist_ok=True)
    cache = data / "cache16k"
    cache.mkdir(exist_ok=True)

    items = [json.loads(l) for l in open(data / "train.jsonl")]
    if args.sources:
        prefixes = args.sources.split(",")
        items = [i for i in items if any(i["source"].startswith(p) for p in prefixes)]
    for it in items:
        it["notes_list"] = read_notes(it["notes"])
    print(f"{len(items)} recordings, {sum(len(i['audio']) for i in items)} variants", flush=True)
    print("caching audio at 16 kHz...", flush=True)
    for it in items:
        for a in it["audio"]:
            load_16k(a, cache)
    val = []
    for line in open(data / "eval_egdb.tsv"):
        a, n, g, *_ = line.rstrip("\n").split("\t")
        if g in ("egdb/DI", "egdb/Plexi", "egdb/real"):
            val.append({"audio": a, "notes_list": read_notes(n), "group": g})

    device = "cuda"
    model = GuitarCRNN()
    state = torch.load(args.init, map_location="cpu", weights_only=False)
    state = state.get("model", state)
    state = {k: v for k, v in state.items() if not k.endswith("num_batches_tracked")}
    model.load_state_dict(state, strict=True)
    torch.backends.cuda.matmul.allow_tf32 = True
    torch.backends.cudnn.allow_tf32 = True
    torch.backends.cudnn.benchmark = True
    model.to(device).train()
    freeze_bn(model)
    if args.checkpoint:
        checkpoint_acoustic_models(model)
    # The STFT and mel filters are fixed signal processing.
    for name, p in model.named_parameters():
        if name.startswith(("spectrogram_extractor", "logmel_extractor")):
            p.requires_grad_(False)
    params = [p for p in model.parameters() if p.requires_grad]
    opt = torch.optim.AdamW(params, lr=args.lr, weight_decay=1e-4)
    sched = torch.optim.lr_scheduler.LambdaLR(
        opt, lambda s: min(1.0, (s + 1) / 300) * 0.5 * (1 + math.cos(math.pi * min(s, args.steps) / args.steps))
    )
    loader = torch.utils.data.DataLoader(
        Clips(items, cache, args.steps * args.batch * args.accum, args.seconds),
        batch_size=args.batch,
        num_workers=args.workers,
        persistent_workers=True,
    )
    best = validate(model, val, cache, device)
    print(f"step 0 val onset F1 {best:.4f}", flush=True)
    log = open(out / "log.jsonl", "a")
    log.write(json.dumps({"step": 0, "val_f1": best}) + "\n")
    torch.save({"model": model.state_dict()}, out / "model.pt")
    t0, running = time.time(), 0.0
    batches = iter(loader)
    for step in range(1, args.steps + 1):
        opt.zero_grad(set_to_none=True)
        for _ in range(args.accum):
            batch = [b.to(device, non_blocking=True) for b in next(batches)]
            # fp32 (TF32 matmuls): fp16 overflows in the power spectrum, and
            # the GRUs have no fast bf16 path.
            outputs = model(batch[0])
            loss = loss_fn([o.clamp(1e-7, 1 - 1e-7) for o in outputs], batch)
            (loss / args.accum).backward()
        torch.nn.utils.clip_grad_norm_(params, 1.0)
        opt.step()
        sched.step()
        running = 0.98 * running + 0.02 * loss.item() if step > 1 else loss.item()
        if step % 100 == 0:
            print(f"step {step} loss {running:.4f} lr {sched.get_last_lr()[0]:.2e} {(time.time() - t0) / step:.2f}s/step", flush=True)
        if step % args.val_every == 0 or step == args.steps:
            f1 = validate(model, val, cache, device)
            print(f"step {step} val onset F1 {f1:.4f} (best {best:.4f})", flush=True)
            log.write(json.dumps({"step": step, "loss": running, "val_f1": f1}) + "\n")
            log.flush()
            if f1 > best:
                best = f1
                torch.save({"model": model.state_dict()}, out / "model.pt")
    model.load_state_dict(torch.load(out / "model.pt", weights_only=False)["model"])
    export(model, out / "model.onnx")
    print(f"best val onset F1 {best:.4f}; exported {out / 'model.onnx'}", flush=True)


if __name__ == "__main__":
    main()
