"""Guitar separation benchmark: runs a separator over the labelled band mixes
(`make_mixes.py`, manifest `eval_mix.tsv`) and writes the guitar stems plus a
manifest `eval_mix_<separator>.tsv` (same notes, group `sep-<separator>/...`),
which `bench_notes.py --manifests` then scores with any transcription model.
The separator is judged by the transcription of its stem, not by SDR.

Separators:
- `htdemucs_6s`: Hybrid Transformer Demucs, 6 sources (Rouard et al. 2023;
  MIT), through the `demucs` package.
- `bsrofo_sw`: BS-RoFormer SW 6-stem checkpoint (community model, license
  unknown; local evaluation only), through ZFTurbo's MSST code
  (`--msst` points at a clone).

Usage:
  python bench_separation.py --data PREPARED --out STEMS --separator htdemucs_6s [--limit 12]
  python bench_separation.py --data PREPARED --out STEMS --separator bsrofo_sw \\
      --msst ~/gva/MSST --config cfg.yaml --ckpt model.ckpt
"""
import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench_notes import spread  # noqa: E402


def load_items(data, limit):
    groups = {}
    for line in open(data / "eval_mix.tsv"):
        audio, notes, group, *_ = line.rstrip("\n").split("\t")
        groups.setdefault(group, []).append((audio, notes, group))
    out = []
    for g in sorted(groups):
        out += spread(groups[g], limit)
    return out


class Demucs:
    rate = 44100

    def __init__(self, device):
        from demucs.pretrained import get_model

        self.model = get_model("htdemucs_6s").to(device).eval()
        self.device = device
        self.index = self.model.sources.index("guitar")

    def guitar(self, x):  # x: [2, T] float32 at 44.1 kHz
        from demucs.apply import apply_model

        with torch.no_grad():
            wav = torch.from_numpy(x)[None].to(self.device)
            ref = wav.mean(1, keepdim=True)
            mean, std = ref.mean(), ref.std() + 1e-8
            out = apply_model(self.model, (wav - mean) / std, shifts=1, overlap=0.25, device=self.device)
            return (out[0, self.index] * std + mean).cpu().numpy()


class MsstRoformer:
    rate = 44100

    def __init__(self, msst, config, ckpt, device):
        sys.path.insert(0, str(msst))
        from utils.model_utils import demix
        from utils.settings import get_model_from_config

        self.demix = demix
        self.model, self.config = get_model_from_config("bs_roformer", str(config))
        state = torch.load(ckpt, map_location="cpu", weights_only=False)
        state = state.get("state_dict", state.get("state", state))
        self.model.load_state_dict(state)
        self.model = self.model.to(device).eval()
        self.device = device

    def guitar(self, x):
        with torch.no_grad():
            stems = self.demix(self.config, self.model, x, self.device, model_type="bs_roformer", pbar=False)
        return np.asarray(stems["guitar"], np.float32)


def stereo_44k(path):
    from scipy.signal import resample_poly

    x, sr = sf.read(path, dtype="float32", always_2d=True)
    x = x.T
    if x.shape[0] == 1:
        x = np.repeat(x, 2, 0)
    if sr != 44100:
        g = np.gcd(sr, 44100)
        x = resample_poly(x, 44100 // g, sr // g, axis=1).astype(np.float32)
    return x[:2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--separator", required=True, choices=["htdemucs_6s", "bsrofo_sw"])
    ap.add_argument("--limit", type=int, default=12)
    ap.add_argument("--msst", type=Path)
    ap.add_argument("--config", type=Path)
    ap.add_argument("--ckpt", type=Path)
    ap.add_argument("--device", default="cuda")
    args = ap.parse_args()
    data, out = args.data.expanduser(), args.out.expanduser() / args.separator
    out.mkdir(parents=True, exist_ok=True)
    if args.separator == "htdemucs_6s":
        sep = Demucs(args.device)
    else:
        sep = MsstRoformer(args.msst.expanduser(), args.config.expanduser(), args.ckpt.expanduser(), args.device)
    rows, timing = [], open(out / "timing.jsonl", "a")
    items = load_items(data, args.limit)
    for k, (audio, notes, group) in enumerate(items):
        dst = out / (group.replace("/", "_") + "__" + Path(audio).stem + ".wav")
        if not dst.exists():
            x = stereo_44k(audio)
            t0 = time.time()
            g = sep.guitar(x)
            dt = time.time() - t0
            sf.write(dst, g.T, 44100, subtype="FLOAT")
            timing.write(json.dumps({"item": dst.name, "audio_s": x.shape[1] / 44100, "wall_s": dt}) + "\n")
            timing.flush()
            print(f"[{k + 1}/{len(items)}] {dst.name}: {x.shape[1] / 44100:.0f}s in {dt:.1f}s", flush=True)
        rows.append(f"{dst}\t{notes}\tsep-{args.separator}/{group.removeprefix('mix/')}")
    (data / f"eval_mix_{args.separator}.tsv").write_text("\n".join(rows) + "\n")
    print(f"wrote {data / f'eval_mix_{args.separator}.tsv'} ({len(rows)} items)")


if __name__ == "__main__":
    main()
