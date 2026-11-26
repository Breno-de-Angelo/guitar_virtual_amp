"""Guitar note labels from MulTTiPop (Pruyne et al. 2026, CC BY 4.0,
https://huggingface.co/datasets/gclef-cmu/multtipop): 572 segments of
commercial pop and rock songs (22 s on average) with multitrack MIDI from the
Lakh MIDI Dataset, beat-aligned to the recording and checked by annotators.
The MIDI files are fan transcriptions, timing inside a beat is interpolated,
and the guitar parts may be simplified: a weaker reference than RWC-P's,
on far more varied, well-known songs. The authors ask for evaluation only,
never training, and for downloading only the labelled segment of the audio.

Labels use `prepare_rwc.py`'s rules (guitar programs 24-31, MIDI 36-96,
>= 20 ms, unisons once, kind from the programs). Segments with fewer than
`--min-notes` guitar notes are left out.

The audio is on YouTube only. `--download` fetches each labelled segment
with yt-dlp (`GVA_YTDLP`, the app's tools venv, or PATH). YouTube's terms
forbid downloading without permission: that step is opt-in and the user's
decision; without it the script only writes the labels.

Usage: python prepare_multtipop.py --out ~/.cache/gva_test/multtipop [--download]
writes <out>/midi/, <out>/notes/<id>.txt, <out>/eval_multtipop.tsv (only
segments whose audio is in <out>/audio/<id>.wav), <out>/segments.tsv.
"""
import argparse
import json
import os
import shutil
import subprocess
import urllib.request
from collections import Counter
from pathlib import Path

from prepare_rwc import ACOUSTIC, DISTORTED, GUITAR, MAX_PITCH, MIN_DUR, MIN_PITCH, dedupe, notes_by_track, write

HF = "https://huggingface.co/datasets/gclef-cmu/multtipop/resolve/main/"


def ytdlp():
    for c in (os.environ.get("GVA_YTDLP"), str(Path.home() / ".local/share/guitar_virtual_amp/tools/ytdlp-venv/bin/yt-dlp"), shutil.which("yt-dlp")):
        if c and Path(c).exists():
            return c
    raise SystemExit("yt-dlp not found (scripts/install_ytdlp.sh)")


def download(seg, dest):
    yt = seg["youtube"]
    if yt.get("timestampType") != "absolute":
        return f"{seg['id']}: relative timestamps, skipped"
    tmp = dest.with_suffix(".dl")
    cmd = [ytdlp(), "-q", "--no-playlist", "-f", "bestaudio", "-x", "--audio-format", "wav",
           "--download-sections", f"*{yt['start']:.3f}-{yt['end']:.3f}", "--force-keyframes-at-cuts",
           "-o", str(tmp) + ".%(ext)s", f"https://www.youtube.com/watch?v={yt['ytid']}"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    got = Path(str(tmp) + ".wav")
    if r.returncode != 0 or not got.exists():
        return f"{seg['id']}: {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else 'failed'}"
    got.rename(dest)
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--min-notes", type=int, default=20)
    ap.add_argument("--download", action="store_true", help="fetch the labelled audio segments from YouTube (opt-in)")
    args = ap.parse_args()
    out = args.out.expanduser()
    (out / "midi").mkdir(parents=True, exist_ok=True)
    (out / "audio").mkdir(exist_ok=True)
    meta_path = out / "metadata.json"
    if not meta_path.exists():
        urllib.request.urlretrieve(HF + "metadata.json", meta_path)
    segs = json.load(open(meta_path))
    rows, listing, kinds = [], [], Counter()
    for seg in segs:
        mid = out / "midi" / f"{seg['id']}.mid"
        if not mid.exists():
            urllib.request.urlretrieve(HF + seg["labels"].lstrip("./"), mid)
        notes = dedupe([n for v in notes_by_track(mid).values() for n in v
                        if n[4] in GUITAR and MIN_PITCH <= n[2] <= MAX_PITCH and n[1] - n[0] >= MIN_DUR])
        if len(notes) < args.min_notes:
            continue
        progs = Counter(n[4] for n in notes)
        dist, acou = sum(progs[p] for p in DISTORTED), sum(progs[p] for p in ACOUSTIC)
        kind = "distorted" if dist > len(notes) / 2 else "acoustic" if acou > len(notes) / 2 else "clean"
        kinds[kind] += 1
        nf = out / "notes" / f"{seg['id']}.txt"
        write(nf, notes)
        audio = out / "audio" / f"{seg['id']}.wav"
        if args.download and not audio.exists():
            msg = download(seg, audio)
            if msg:
                print(msg, flush=True)
        listing.append(f"{seg['id']}\t{seg['split']}\t{kind}\t{len(notes)}\t{seg['artist']}\t{seg['name']}\t{seg['youtube']['ytid']}\t{seg['youtube']['start']:.3f}\t{seg['youtube']['end']:.3f}")
        if audio.exists():
            rows.append(f"{audio}\t{nf}\tmtp-{seg['split']}/{kind}\t")
    (out / "segments.tsv").write_text("id\tsplit\tkind\tguitar_notes\tartist\ttitle\tytid\tstart\tend\n" + "\n".join(listing) + "\n")
    (out / "eval_multtipop.tsv").write_text("\n".join(rows) + ("\n" if rows else ""))
    print(f"{len(listing)} segments with guitar ({dict(kinds)}); {len(rows)} with audio -> {out / 'eval_multtipop.tsv'}")


if __name__ == "__main__":
    main()
