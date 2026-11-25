#!/usr/bin/env python3
"""Source separation with a BS-RoFormer checkpoint through ZFTurbo's
Music-Source-Separation-Training code (MSST, MIT,
https://github.com/ZFTurbo/Music-Source-Separation-Training), for the app's
transcription pipeline (guitar_core/src/transcribe/separation.rs runs it).
scripts/install_bs_roformer.sh copies it into the MSST environment.

    python separate_msst.py --msst DIR --config YAML --checkpoint CKPT \
        [--device cuda|cpu] [--chunk-size N] [--overlap N] INPUT.wav OUT_DIR

INPUT is a WAV at the model's sample rate (44.1 kHz; the app decodes and
resamples the song first). Mono input is duplicated to stereo. Writes
OUT_DIR/<instrument>.wav (stereo, 32-bit float) for every instrument of the
model. Progress goes to stderr as tqdm lines ("NN%|"), which the app parses.
--chunk-size (samples) and --overlap override the config's inference
settings: smaller chunks need less GPU memory. A CUDA out-of-memory error
exits non-zero with PyTorch's message; the app then runs it again with
--device cpu.
"""
import argparse
import sys
import time
from pathlib import Path


def log(text):
    print(text, file=sys.stderr, flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--msst", type=Path, required=True, help="MSST source folder")
    ap.add_argument("--config", type=Path, required=True)
    ap.add_argument("--checkpoint", type=Path, required=True)
    ap.add_argument("--model-type", default="bs_roformer")
    ap.add_argument("--device", default=None, help="cuda or cpu (default: cuda when available)")
    ap.add_argument("--chunk-size", type=int, default=None)
    ap.add_argument("--overlap", type=int, default=None)
    ap.add_argument("input", type=Path)
    ap.add_argument("out_dir", type=Path)
    args = ap.parse_args()

    import numpy as np
    import soundfile as sf
    import torch

    sys.path.insert(0, str(args.msst))
    from utils.model_utils import demix
    from utils.settings import get_model_from_config

    device = args.device or ("cuda" if torch.cuda.is_available() else "cpu")
    started = time.time()
    model, config = get_model_from_config(args.model_type, str(args.config))
    if args.chunk_size:
        config.audio.chunk_size = args.chunk_size
        if "chunk_size" in config.inference:
            config.inference.chunk_size = args.chunk_size
    if args.overlap:
        config.inference.num_overlap = args.overlap
    state = torch.load(args.checkpoint, map_location="cpu", weights_only=True)
    state = state.get("state_dict", state.get("state", state))
    model.load_state_dict(state)
    model = model.to(device).eval()
    rate = int(config.audio.sample_rate)
    log(f"model {args.checkpoint.name} on {device} in {time.time() - started:.1f}s")

    mix, sr = sf.read(args.input, dtype="float32", always_2d=True)
    if sr != rate:
        sys.exit(f"{args.input}: {sr} Hz, the model needs {rate} Hz")
    mix = mix.T
    if mix.shape[0] == 1:
        mix = np.repeat(mix, 2, 0)
    mix = mix[:2]

    log("  0%|")
    started = time.time()
    stems = demix(config, model, mix, device, model_type=args.model_type, pbar=True)
    seconds = mix.shape[1] / rate
    log(f"100%| separated {seconds:.0f}s of audio in {time.time() - started:.1f}s")
    if device.startswith("cuda"):
        peak = torch.cuda.max_memory_allocated() / 2**20
        log(f"peak CUDA memory allocated: {peak:.0f} MiB")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    for name, audio in stems.items():
        audio = np.asarray(audio, np.float32)
        part = args.out_dir / f"{name}.wav.part"
        sf.write(part, audio.T, rate, subtype="FLOAT", format="WAV")
        part.rename(args.out_dir / f"{name}.wav")


if __name__ == "__main__":
    main()
