"""Real-mix benchmark items from MoisesDB (Pereira et al., ISMIR 2023): for
every song with guitar, the 60 s window where guitar plays the most, written
as the full mix, the sum of its guitar tracks, and (songs with 2+ guitars)
each guitar track alone. Reads the stems straight from `moisesdb.zip`.

MoisesDB has no note labels. The benchmark (`bench_moisesdb.py`) compares a
model's notes on the mix (or on a separator's guitar stem) with notes
transcribed from the isolated guitars, so it measures what the mix costs on
real productions, where our `make_mixes.py` mixes put a guitar over an
unrelated song.

Writes into OUT:
- `audio/<song>__{mix,guitar,g<k>}.flac` (44.1 kHz stereo, 24 bit; all
  files of a song share one gain, which keeps the mix under 0.99 peak, so the
  isolated guitars are exactly what the mix contains)
- `moises_mix.tsv`, `moises_iso.tsv`: `audio<TAB>-<TAB>group` manifests for
  `bench_notes.py run` (no notes: references come from the isolated
  guitars). Group `moises-{mix,iso}/<kind>`, kind = `distorted` when a
  distorted electric guitar plays, else `clean`, else `acoustic`.
- `moises_parts.tsv`: per-guitar tracks of songs with several guitars.
- `songs.jsonl`: metadata (artist, title, genre, window, guitar tracks with
  type, pan and share of the window's guitar energy).

Usage:
  python prepare_moisesdb.py --zip ~/Downloads/moisesdb.zip --out ~/.cache/gva_test/prepared_moises
MoisesDB's terms apply (check them before training anything you'd ship).
"""
import argparse
import io
import json
import zipfile
from pathlib import Path

import numpy as np
import soundfile as sf

RATE = 44100
WINDOW_S = 60
KINDS = ("distorted_electric_guitar", "clean_electric_guitar", "acoustic_guitar")


def read(z, name):
    x, sr = sf.read(io.BytesIO(z.read(name)), dtype="float32", always_2d=True)
    assert sr == RATE, (name, sr)
    if x.shape[1] == 1:
        x = np.repeat(x, 2, 1)
    return x[:, :2]


def add(acc, x):
    if acc is None:
        return x.copy()
    n = max(len(acc), len(x))
    out = np.zeros((n, 2), np.float32)
    out[: len(acc)] += acc
    out[: len(x)] += x
    return out


def best_window(guitar):
    """Start sample of the WINDOW_S window with the most seconds of active
    guitar (RMS above 10% of the song's 95th-percentile second), ties broken
    by energy."""
    n = len(guitar) // RATE
    w = min(WINDOW_S, n)
    if n <= w:
        return 0, len(guitar)
    rms = np.sqrt((guitar[: n * RATE] ** 2).mean(1).reshape(n, RATE).mean(1))
    active = rms > 0.1 * np.percentile(rms, 95)
    act = np.convolve(active, np.ones(w), "valid")
    eng = np.convolve(rms**2, np.ones(w), "valid")
    best = int(np.lexsort((eng, act))[-1])
    return best * RATE, (best + w) * RATE


MIN_SHARE = 0.02  # guitar parts quieter than this in the window are skipped


def write(path, x, gain):
    sf.write(path, np.clip(x * gain, -1.0, 1.0), RATE, subtype="PCM_24", format="FLAC")


def pan(x):
    """-1 (left) .. 1 (right), from channel energies."""
    l, r = float((x[:, 0] ** 2).sum()), float((x[:, 1] ** 2).sum())
    return (r - l) / max(l + r, 1e-12)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--zip", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--limit", type=int, default=0, help="songs (0 = all), for testing")
    args = ap.parse_args()
    out = args.out.expanduser()
    (out / "audio").mkdir(parents=True, exist_ok=True)
    (out / "cache16k").mkdir(exist_ok=True)  # bench_notes.py caches 16 kHz audio here
    z = zipfile.ZipFile(args.zip.expanduser())
    songs = sorted(n for n in z.namelist() if n.endswith("/data.json"))
    if args.limit:
        songs = songs[: args.limit]
    rows = {"mix": [], "iso": [], "parts": []}
    meta = open(out / "songs.jsonl", "w")
    for k, name in enumerate(songs):
        base = name.rsplit("/", 1)[0]
        song = base.rsplit("/", 1)[1]
        d = json.load(z.open(name))
        guitars = [(s["stemName"], t) for s in d["stems"] for t in s["tracks"] if s["stemName"] == "guitar"]
        if not guitars:
            continue
        mix, gsum, tracks = None, None, []
        for s in d["stems"]:
            for t in s["tracks"]:
                x = read(z, f"{base}/{s['stemName']}/{t['id']}.{t.get('extension', 'wav')}")
                mix = add(mix, x)
                if s["stemName"] == "guitar":
                    gsum = add(gsum, x)
                    tracks.append((t, x))
        a, b = best_window(gsum)
        types = [t["trackType"] for t, _ in tracks]
        kind = next((c.split("_")[0] for c in KINDS if c in types), "other")
        ids = {"mix": f"{song}__mix.flac", "iso": f"{song}__guitar.flac"}
        peak = max(float(np.abs(mix[a:b]).max()), float(np.abs(gsum[a:b]).max()), 1e-9)
        gain = min(1.0, 0.99 / peak)
        write(out / "audio" / ids["mix"], mix[a:b], gain)
        write(out / "audio" / ids["iso"], gsum[a:b], gain)
        rows["mix"].append(f"{out / 'audio' / ids['mix']}\t-\tmoises-mix/{kind}")
        rows["iso"].append(f"{out / 'audio' / ids['iso']}\t-\tmoises-iso/{kind}")
        total = float((gsum[a:b] ** 2).sum()) or 1.0
        info = []
        for j, (t, x) in enumerate(tracks):
            seg = x[a:b]
            share = float((seg**2).sum()) / total
            info.append({"type": t["trackType"], "pan": round(pan(seg), 3), "share": round(share, 3),
                         "has_bleed": t.get("has_bleed", False)})
            if len(tracks) > 1 and share >= MIN_SHARE:
                f = out / "audio" / f"{song}__g{j}.flac"
                write(f, seg, gain)
                rows["parts"].append(f"{f}\t-\tmoises-part/{t['trackType']}\t{song}")
        meta.write(json.dumps({"song": song, "artist": d.get("artist"), "title": d.get("song"),
                               "genre": d.get("genre"), "kind": kind, "start_s": a / RATE,
                               "secs": (b - a) / RATE, "guitars": info}) + "\n")
        meta.flush()
        print(f"[{k + 1}/{len(songs)}] {d.get('artist')} - {d.get('song')}: {kind}, {len(tracks)} guitar(s), "
              f"window {a / RATE:.0f}-{b / RATE:.0f}s", flush=True)
    for key, name in [("mix", "moises_mix.tsv"), ("iso", "moises_iso.tsv"), ("parts", "moises_parts.tsv")]:
        (out / name).write_text("\n".join(rows[key]) + "\n")
    print(f"{len(rows['iso'])} songs, {len(rows['parts'])} guitar parts -> {out}")


if __name__ == "__main__":
    main()
