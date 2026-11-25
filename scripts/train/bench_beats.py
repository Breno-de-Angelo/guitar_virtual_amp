"""Beat and downbeat tracking benchmark on GuitarSet players 03-05 (solo
guitar with annotated beat positions in the JAMS files).

Trackers:
- `beat_this:<checkpoint>`: Beat This! (Foscarin et al. 2024, MIT),
  minimal peak-picking post-processing (no DBN), e.g. small0, final0.
- `librosa`: dynamic-programming beat tracker (Ellis 2007), beats only.

Metrics (mir_eval, mean per excerpt, the first 5 s trimmed as is standard):
beat F-measure (70 ms window), beat CMLt, downbeat F-measure.

Usage: python bench_beats.py --guitarset GUITARSET --tracker beat_this:final0 [--limit 0]
"""
import argparse
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench_notes import spread  # noqa: E402


def truth(jams):
    doc = json.load(open(jams))
    beats, downs = [], []
    for a in doc["annotations"]:
        if a["namespace"] == "beat_position":
            for b in a["data"]:
                beats.append(b["time"])
                if b["value"]["position"] == 1:
                    downs.append(b["time"])
    return np.array(sorted(beats)), np.array(sorted(downs))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--guitarset", type=Path, required=True)
    ap.add_argument("--tracker", action="append", required=True)
    ap.add_argument("--limit", type=int, default=0, help="excerpts per comp/solo group (0 = all 90 each)")
    ap.add_argument("--device", default="cuda")
    args = ap.parse_args()
    import mir_eval

    gs = args.guitarset.expanduser()
    groups = {}
    for jams in sorted((gs / "annotation").glob("0[345]_*.jams")):
        groups.setdefault("comp" if jams.stem.endswith("_comp") else "solo", []).append(jams)
    items = [(g, j) for g in sorted(groups) for j in spread(groups[g], args.limit)]
    print(f"{len(items)} excerpts")
    print("| tracker | group | beat F | beat CMLt | downbeat F |\n|---|---|---|---|---|")
    for tracker in args.tracker:
        if tracker.startswith("beat_this:"):
            from beat_this.inference import File2Beats

            f2b = File2Beats(checkpoint_path=tracker.split(":", 1)[1], device=args.device, dbn=False)
            track = lambda p: f2b(str(p))  # noqa: E731
        elif tracker == "librosa":
            import librosa

            def track(p):
                y, sr = librosa.load(str(p), sr=22050)
                _, frames = librosa.beat.beat_track(y=y, sr=sr, units="time")
                return np.asarray(frames), None
        else:
            raise SystemExit(f"unknown tracker {tracker}")
        res = {}
        for g, jams in items:
            ref_b, ref_d = truth(jams)
            est_b, est_d = track(gs / "audio" / (jams.stem + "_mic.wav"))
            rb, eb = mir_eval.beat.trim_beats(ref_b), mir_eval.beat.trim_beats(np.asarray(est_b))
            row = [mir_eval.beat.f_measure(rb, eb), mir_eval.beat.continuity(rb, eb)[1]]
            if est_d is not None:
                row.append(mir_eval.beat.f_measure(mir_eval.beat.trim_beats(ref_d), mir_eval.beat.trim_beats(np.asarray(est_d))))
            res.setdefault(g, []).append(row)
        for g in sorted(res):
            m = np.mean(res[g], 0)
            down = f"{m[2]:.3f}" if len(m) > 2 else "-"
            print(f"| {tracker} | {g} | {m[0]:.3f} | {m[1]:.3f} | {down} |", flush=True)


if __name__ == "__main__":
    main()
