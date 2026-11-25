"""Training throughput probe for the guitar CRNN (finetune_crnn.py recipe):
synthetic batches (GPU cost only) or the real loader (adds data loading).
Usage: python probe_throughput.py --init guitar-gaps.pth --batch 4 --accum 2 [--real DATA]
       [--steps 30] [--bf16] [--train-bn]
Prints one JSON line: seconds per optimizer step, clips and audio seconds per second, peak GPU
memory.
"""
import argparse
import json
import sys
import time
from pathlib import Path

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import finetune_crnn as ft  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--init", type=Path, required=True)
ap.add_argument("--batch", type=int, default=4)
ap.add_argument("--accum", type=int, default=2)
ap.add_argument("--seconds", type=float, default=4.0)
ap.add_argument("--steps", type=int, default=30)
ap.add_argument("--warmup", type=int, default=5)
ap.add_argument("--real", type=Path, default=None, help="prepared data dir: use the real loader")
ap.add_argument("--workers", type=int, default=6)
ap.add_argument("--train-bn", action="store_true")
ap.add_argument("--bf16", action="store_true", help="autocast bf16 (STFT stays fp32 inside autocast-safe ops)")
args = ap.parse_args()

dev = "cuda"
torch.backends.cuda.matmul.allow_tf32 = True
torch.backends.cudnn.allow_tf32 = True
torch.backends.cudnn.benchmark = True
model = ft.GuitarCRNN()
state = torch.load(args.init, map_location="cpu", weights_only=False)
state = state.get("model", state)
model.load_state_dict({k: v for k, v in state.items() if not k.endswith("num_batches_tracked")})
model.to(dev).train()
if not args.train_bn:
    ft.freeze_bn(model)
for name, p in model.named_parameters():
    if name.startswith(("spectrogram_extractor", "logmel_extractor")):
        p.requires_grad_(False)
params = [p for p in model.parameters() if p.requires_grad]
opt = torch.optim.AdamW(params, lr=1e-5)

samples = int(args.seconds * ft.SAMPLE_RATE)
frames = samples // (ft.SAMPLE_RATE // ft.FPS) + 1
if args.real:
    items = [json.loads(l) for l in open(args.real / "train.jsonl")]
    for it in items:
        it["notes_list"] = ft.read_notes(it["notes"])
    total = (args.steps + args.warmup) * args.batch * args.accum
    loader = iter(torch.utils.data.DataLoader(
        ft.Clips(items, args.real / "cache16k", total, args.seconds),
        batch_size=args.batch, num_workers=args.workers, persistent_workers=True))
    next_batch = lambda: [b.to(dev, non_blocking=True) for b in next(loader)]  # noqa: E731
else:
    def next_batch():
        x = torch.randn(args.batch, samples, device=dev) * 0.1
        rolls = [torch.rand(args.batch, frames, ft.PITCHES, device=dev).round() for _ in range(5)]
        return [x] + rolls

torch.cuda.reset_peak_memory_stats()
for step in range(args.warmup + args.steps):
    if step == args.warmup:
        torch.cuda.synchronize()
        t0 = time.time()
    opt.zero_grad(set_to_none=True)
    for _ in range(args.accum):
        batch = next_batch()
        with torch.autocast("cuda", dtype=torch.bfloat16, enabled=args.bf16):
            out = model(batch[0])
        out = [o.float() for o in out]
        loss = ft.loss_fn([o.clamp(1e-7, 1 - 1e-7) for o in out], batch)
        (loss / args.accum).backward()
    torch.nn.utils.clip_grad_norm_(params, 1.0)
    opt.step()
torch.cuda.synchronize()
dt = (time.time() - t0) / args.steps
clips = args.batch * args.accum
print(json.dumps({
    "host": __import__("socket").gethostname(), "mode": "real" if args.real else "synthetic",
    "batch": args.batch, "accum": args.accum, "seconds": args.seconds, "train_bn": args.train_bn, "bf16": args.bf16,
    "s_per_step": round(dt, 3), "clips_per_s": round(clips / dt, 2),
    "audio_s_per_s": round(clips * args.seconds / dt, 1),
    "peak_gb": round(torch.cuda.max_memory_allocated() / 1e9, 2),
}), flush=True)
