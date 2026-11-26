"""Audio features for the audio-conditioned fingering tagger (roadmap T2,
step 2): the same pitch sounds different on a thick and a thin string, so
the tagger also gets a pitch-relative spectrogram patch at each note.

Spectrogram (kept simple so the app can compute exactly the same thing):
mono audio at 16 kHz, peak-normalised; frames every 160 samples (10 ms),
frame i centred on sample 160 i, a 2048-sample periodic Hann window, zeros
outside the signal; |rfft| (1025 bins) through a log-frequency filterbank of
`N_BINS` triangles at MIDI pitch 24 + k/3 (three per semitone, up to MIDI
119), each `max(f (2^(1/36) - 1), 7.8125 Hz)` wide on each side and
normalised to sum 1; then natural log of (1e-5 + value). Stored as uint8:
q = clip(round((log + 10) * 15), 0, 255).

Patch of a note (pitch p, onset t): bins 3 (p - 24) - 3 ... + 147 (one
semitone below the fundamental to four octaves above), frames
round(100 t) - 3 ... + 25 (30 ms before to 250 ms after the onset); outside
the spectrogram reads as q = 0. The model subtracts the patch's maximum.

Usage (laptop, where all the audio is):
  python audio.py --root ~/.cache/gva_test --data data.json --out ~/.cache/gva_test/fingering_audio
writes `<out>/spec/<hash>.npy` (uint8 [frames, N_BINS]) and `<out>/index.json`:
{recording id: [{"tone", "spec", "shift"}, ...]} (`shift` in seconds, added to
the label times: Guitar-TECHS and GOAT labels are 20-40 ms off).
"""
import argparse
import hashlib
import json
import math
import os
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np

SR = 16000
HOP = 160
N_FFT = 2048
LOW_MIDI = 24
BPS = 3  # bins per semitone
N_BINS = BPS * (119 - LOW_MIDI) + 1  # 286
P_LO, P_HI = -3, 147  # patch bins relative to the fundamental
T_LO, T_HI = -3, 25  # patch frames relative to the onset
PATCH_F, PATCH_T = P_HI - P_LO, T_HI - T_LO  # 150 x 28
Q_OFF, Q_SCALE = 10.0, 15.0


def filterbank():
    """[N_BINS, N_FFT // 2 + 1] triangular log-frequency weights."""
    df = SR / N_FFT
    freqs = np.arange(N_FFT // 2 + 1) * df
    fb = np.zeros((N_BINS, len(freqs)), np.float64)
    for k in range(N_BINS):
        fc = 440.0 * 2 ** ((LOW_MIDI + k / BPS - 69) / 12)
        hw = max(fc * (2 ** (1 / 36) - 1), df)
        w = np.maximum(0.0, 1.0 - np.abs(freqs - fc) / hw)
        fb[k] = w / w.sum()
    return fb.astype(np.float32)


FB = None


def spectrogram(x):
    """uint8 [frames, N_BINS] of mono float audio at 16 kHz."""
    global FB
    if FB is None:
        FB = filterbank()
    x = np.asarray(x, np.float32)
    peak = float(np.abs(x).max()) if len(x) else 0.0
    if peak > 0:
        x = x / peak
    n = len(x) // HOP + 1
    pad = np.concatenate([np.zeros(N_FFT // 2, np.float32), x, np.zeros(N_FFT // 2 + HOP, np.float32)])
    win = (0.5 - 0.5 * np.cos(2 * np.pi * np.arange(N_FFT) / N_FFT)).astype(np.float32)
    out = np.empty((n, N_BINS), np.uint8)
    for s in range(0, n, 2048):  # chunks keep memory small
        idx = np.arange(s, min(n, s + 2048))
        frames = np.lib.stride_tricks.sliding_window_view(pad, N_FFT)[idx * HOP] * win
        mag = np.abs(np.fft.rfft(frames, axis=1)).astype(np.float32)
        lg = np.log(1e-5 + mag @ FB.T)
        out[idx] = np.clip(np.round((lg + Q_OFF) * Q_SCALE), 0, 255).astype(np.uint8)
    return out


def patches(spec, onsets, pitches):
    """uint8 [N, PATCH_F, PATCH_T] patches (frequency first)."""
    onsets = np.asarray(onsets, np.float64)
    pitches = np.asarray(pitches, np.int64)
    n = len(onsets)
    frames, bins = spec.shape
    t0 = np.round(onsets * (SR / HOP)).astype(np.int64) + T_LO
    f0 = BPS * (pitches - LOW_MIDI) + P_LO
    ti = t0[:, None] + np.arange(PATCH_T)[None, :]  # [N, T]
    fi = f0[:, None] + np.arange(PATCH_F)[None, :]  # [N, F]
    tv = (ti >= 0) & (ti < frames)
    fv = (fi >= 0) & (fi < bins)
    out = spec[np.clip(ti, 0, frames - 1)[:, None, :], np.clip(fi, 0, bins - 1)[:, :, None]]
    return np.where(fv[:, :, None] & tv[:, None, :], out, 0).astype(np.uint8).reshape(n, PATCH_F, PATCH_T)


# ---------------------------------------------------------------- preparation

def load_16k(path, caches):
    """Mono float32 at 16 kHz; reuses finetune_crnn.py's int16 caches."""
    for c in caches:
        key = c / (hashlib.sha1(str(path).encode()).hexdigest() + ".npy")
        if key.exists():
            return np.asarray(np.load(key, mmap_mode="r"), np.float32) / 32768.0
    import soundfile as sf
    from scipy.signal import resample_poly

    x, sr = sf.read(path, dtype="float32", always_2d=True)
    x = x.mean(axis=1)
    if sr != SR:
        g = math.gcd(sr, SR)
        x = resample_poly(x, SR // g, sr // g).astype(np.float32)
    return x


def shifts(root):
    """{notes file stem path: seconds} from align_labels.py's logs."""
    out = {}
    for log in (root / "prepared_gs/label_shifts.tsv", root / "prepared_goat/label_shifts.tsv"):
        if not log.exists():
            continue
        for line in open(log):
            v = line.rstrip("\n").split("\t")
            if len(v) >= 4 and v[3]:
                out[v[0].split("/notes/", 1)[1].rsplit(".", 1)[0]] = float(v[3]) / 1000
    return out


def tones(root, rec, shift_of):
    """[(tone, audio path, shift)] of a data.json recording."""
    rid, src = rec["id"], rec["source"]
    name = rid.split("/", 1)[1]
    out = []
    if src.startswith("guitarset"):
        out.append(("mic", root / f"guitarset/audio/{name}_mic.wav", 0.0))
    elif src == "egdb":
        for amp in ("DI", "Marshall", "Mesa", "Plexi"):
            out.append((amp, root / f"egdb/audio_{amp}/{name}.wav", 0.0))
        for d in sorted((root / "renders/egdb").glob("*")):
            out.append((d.name, d / f"{name}.wav", 0.0))
    elif src == "egset12":
        out.append(("amp", root / f"egset12/{name}.wav", 0.0))
    elif src == "techs":
        part, mid = name.split("/")
        stem = mid.removeprefix("midi_")
        sh = shift_of.get(f"techs/{part}/{stem}", 0.0)
        out.append(("di", root / f"guitar_techs/{part}/audio/directinput/directinput_{stem}.wav", sh))
        out.append(("micamp", root / f"guitar_techs/{part}/audio/micamp/micamp_{stem}.wav", sh))
        for d in sorted((root / "renders/techs").glob("*")):
            out.append((d.name, d / f"{part}__directinput_{stem}.wav", sh))
    elif src == "goat" and rec["split"] == "test":  # only the test split has aligned notes
        sh = shift_of.get(f"goat/{name}", 0.0)
        d = root / f"goat/GOAT/data/{name}"
        out.append(("di", d / f"{name}.wav", sh))
        for k in range(1, 6):
            out.append((f"amp_{k}", d / f"{name}_amp_{k}.wav", sh))
    return [(t, p, s) for t, p, s in out if p.exists()]


def _job(args):
    path, dest, caches = args
    if not dest.exists():
        spec = spectrogram(load_16k(path, caches))
        tmp = dest.with_name(f"{dest.stem}.{os.getpid()}.tmp.npy")
        np.save(tmp, spec)
        os.replace(tmp, dest)
    return str(path)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--jobs", type=int, default=8)
    args = ap.parse_args()
    root = args.root.expanduser()
    (args.out / "spec").mkdir(parents=True, exist_ok=True)
    caches = [root / "prepared_gs/cache16k", root / "prepared/cache16k"]
    shift_of = shifts(root)
    index, jobs = {}, []
    for rec in json.load(open(args.data))["recordings"]:
        entries = []
        for tone, path, sh in tones(root, rec, shift_of):
            h = hashlib.sha1(str(path).encode()).hexdigest()[:20]
            entries.append({"tone": tone, "spec": f"spec/{h}.npy", "shift": sh})
            jobs.append((path, args.out / "spec" / f"{h}.npy", caches))
        if entries:
            index[rec["id"]] = entries
    print(f"{len(index)} recordings, {len(jobs)} audio files", flush=True)
    with ProcessPoolExecutor(args.jobs) as ex:
        for i, _ in enumerate(ex.map(_job, jobs, chunksize=4)):
            if (i + 1) % 200 == 0:
                print(f"{i + 1}/{len(jobs)}", flush=True)
    json.dump(index, open(args.out / "index.json", "w"), indent=0)
    missing = [r["id"] for r in json.load(open(args.data))["recordings"] if r["id"] not in index and not (r["source"] == "goat" and r["split"] != "test")]
    print(f"done; recordings without audio: {len(missing)} {missing[:5]}")


if __name__ == "__main__":
    main()
