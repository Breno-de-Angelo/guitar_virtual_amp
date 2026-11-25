"""Builds band mixes with known guitar notes: real guitar recordings with
note labels (from `prepare_data.py`'s eval manifests) over the drums, bass
and vocals of real songs from MUSDB18 (Rafii et al. 2017, non-commercial
research use). MUSDB's "other" stem is left out: it often holds guitars,
which would have no labels. The backing doesn't share the guitar's key or
tempo, so score the raw model notes; the final tab also depends on the beat
tracker following the guitar.

Each mix is stereo: the guitar a little off center, the backing as mixed,
guitar-to-backing level drawn from --snr (dB, guitar RMS over backing RMS).
Writes `<out>/mixes/*.wav` and `<out>/eval_mix.tsv` (same notes files, group
`mix/<original group>`), for `eval_manifest` with `GVA_EVAL_DEMUCS=1`.

Usage: python make_mixes.py --musdb ~/.cache/gva_test/musdb/musdb18 \\
         --manifest prepared/eval_techs.tsv --manifest prepared/eval_egdb.tsv \\
         --out prepared [--groups music-micamp,egdb/Marshall,egdb/real] [--snr -6,0]
"""

import argparse
import random
import subprocess
from pathlib import Path

import numpy as np
import soundfile as sf

RATE = 44100


def stem(mp4, index):
    """Stereo float32 of stream `index` of a MUSDB .stem.mp4 (1 drums,
    2 bass, 3 other, 4 vocals)."""
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(mp4), "-map", f"0:{index}", "-f", "f32le", "-ac", "2", "-ar", str(RATE), "-"],
        check=True,
        capture_output=True,
    ).stdout
    return np.frombuffer(raw, np.float32).reshape(-1, 2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--musdb", type=Path, required=True)
    ap.add_argument("--manifest", type=Path, action="append", required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--groups", default="")
    ap.add_argument("--snr", default="-6,0")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    lo, hi = (float(v) for v in args.snr.split(","))
    songs = sorted(args.musdb.expanduser().glob("test/*.stem.mp4"))
    if not songs:
        raise SystemExit("no MUSDB test songs found")
    wanted = [g for g in args.groups.split(",") if g]
    out = args.out.expanduser()
    (out / "mixes").mkdir(parents=True, exist_ok=True)
    rows = []
    for manifest in args.manifest:
        for line in open(manifest.expanduser()):
            audio, notes, group, *_ = line.rstrip("\n").split("\t")
            if wanted and not any(w in group for w in wanted):
                continue
            guitar, sr = sf.read(audio, dtype="float32", always_2d=True)
            guitar = guitar.mean(axis=1)
            if sr != RATE:
                from scipy.signal import resample_poly

                g = np.gcd(sr, RATE)
                guitar = resample_poly(guitar, RATE // g, sr // g).astype(np.float32)
            song = rng.choice(songs)
            backing = sum(stem(song, i) for i in (1, 2, 4))
            if len(backing) < len(guitar):
                backing = np.tile(backing, (len(guitar) // len(backing) + 1, 1))
            start = rng.randint(0, len(backing) - len(guitar))
            backing = backing[start : start + len(guitar)]
            rms = lambda x: float(np.sqrt(np.mean(x**2))) + 1e-9
            snr = rng.uniform(lo, hi)
            gain = rms(backing) / rms(guitar) * 10 ** (snr / 20)
            pan = rng.uniform(-0.4, 0.4)
            left, right = np.cos((pan + 1) * np.pi / 4), np.sin((pan + 1) * np.pi / 4)
            mix = backing + np.stack([guitar * gain * left, guitar * gain * right], axis=1)
            mix /= max(1e-6, float(np.abs(mix).max())) / 0.9
            name = f"{Path(audio).stem}__{group.replace('/', '_')}__{song.stem.replace(' ', '_')}.wav"
            path = out / "mixes" / name
            sf.write(path, mix, RATE, subtype="PCM_16")
            rows.append(f"{path}\t{notes}\tmix/{group}\t\n")
            print(f"{name}: snr {snr:+.1f} dB, pan {pan:+.2f}", flush=True)
    with open(out / "eval_mix.tsv", "w") as f:
        f.writelines(rows)
    print(f"{len(rows)} mixes")


if __name__ == "__main__":
    main()
