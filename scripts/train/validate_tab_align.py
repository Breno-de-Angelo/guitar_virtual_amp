"""Validates `align_tabs.py` on recordings whose true note times are known:
EGSet12 (Pedroza et al. 2024, CC BY 4.0: GP tab + JAMS labels) and GOAT
(Loth et al. 2025: GP5 tab + fine-aligned MIDI, off by up to +-40 ms per
item). The tab is aligned to the recording without the labels, then the
aligned tab notes are compared with the labels.

Both sets were played to the tab's own tempo, so on top of the plain pair
(`clean`) the tab is distorted the way a commercial recording differs from
its tab:
- `tempo`: per-bar tempo random walk (sd 3%/bar, clipped to 0.8-1.25) times
  a global factor 0.85-1.15, and 2-6 s of near-silence before the recording;
- `detune`: every tab track one semitone up (the aligner must find -1);
- `mix`: the recording over MUSDB18 drums/bass/vocals (`make_mixes.py`
  output, `--mixes`), with the `tempo` distortion;
- `cut`: 15-25% of the tab's bars removed in the middle (a section the
  recording has and the tab doesn't); labels inside it are not scored;
- `wrong`: another item's tab (should be rejected).

Scored for each run: `dtw` (notes read off the warping path) and, for
`clean`, `tab` (the tab's own timing, which these recordings follow).

Metrics per run: onset error |aligned - label| of tab notes matched to
labels of the same pitch within 300 ms (median, p90, signed median), note
F1 at 50 ms (mir_eval, onset only) and at 300 ms (agreement of tab and
labels regardless of timing: the ceiling).

Usage (CPU only; --jobs processes):
  python validate_tab_align.py --egset12 ~/.cache/gva_test/egset12 --goat ~/.cache/gva_test/goat/GOAT \\
      [--goat-train 12] [--mixes mixes/eval_mix.tsv] --out val --jobs 12
"""
import argparse
import csv
import json
import random
import sys
import time
from collections import defaultdict
from multiprocessing import Pool
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import align_tabs as at  # noqa: E402
import gp_tabs  # noqa: E402

CONDITIONS = ("clean", "tempo", "detune", "mix", "cut", "wrong")


# ---------------------------------------------------------------- data

def jams_notes(path):
    doc = json.load(open(path))
    out = []
    for a in doc["annotations"]:
        if a["namespace"] == "note_midi":
            data = a["data"]
            if isinstance(data, dict):
                data = [dict(time=t, duration=d, value=v) for t, d, v in zip(data["time"], data["duration"], data["value"])]
            out += [(n["time"], n["time"] + n["duration"], int(round(n["value"]))) for n in data]
    return sorted(out)


def midi_label_notes(path):
    from prepare_data import midi_notes

    return [(s, e, p) for s, e, p, _ in midi_notes(path)]


def collect(args):
    items = []
    if args.egset12:
        for jams in sorted(args.egset12.glob("*.jams")):
            items.append({"set": "egset12", "name": jams.stem, "tab": str(jams.with_suffix(".gp")),
                          "audio": {"mic": str(jams.with_suffix(".wav"))}, "labels": ("jams", str(jams))})
    if args.goat:
        rows = list(csv.DictReader(open(args.goat / "metadata.csv")))
        test = [r for r in rows if r["split"].strip() == "test"]
        train = [r for r in rows if r["split"].strip() != "test"]
        have = lambda r: all((args.goat / "data" / r["item"] / f"{r['item']}{x}").exists()  # noqa: E731
                             for x in (".wav", ".gp5", "_fine_aligned.mid"))
        train = [r for r in train if have(r)]
        random.Random(1).shuffle(train)
        for r, split in [(r, "test") for r in test] + [(r, "train") for r in train[: args.goat_train]]:
            it, d = r["item"], args.goat / "data" / r["item"]
            tones = {"di": d / f"{it}.wav"} | {f"amp_{k}": d / f"{it}_amp_{k}.wav" for k in range(1, 6)}
            tones = {k: str(v) for k, v in tones.items() if v.exists()}
            if not tones:
                continue
            items.append({"set": f"goat-{split}", "name": it, "tab": str(d / f"{it}.gp5"), "audio": tones,
                          "labels": ("midi", str(d / f"{it}_fine_aligned.mid"))})
    return items


def labels_of(item):
    kind, path = item["labels"]
    return jams_notes(path) if kind == "jams" else midi_label_notes(path)


def mixes_of(manifest):
    """make_mixes.py rows: mix audio by the label file it was made from."""
    out = {}
    if manifest and Path(manifest).exists():
        for line in open(manifest):
            audio, notes, group, *_ = line.rstrip("\n").split("\t")
            out[notes] = audio
    return out


# ---------------------------------------------------------------- distortions

def perturb_tempo(tab, rng):
    occ = tab.occurrences()
    g = rng.uniform(0.85, 1.15)
    f, logf = [], 0.0
    for _ in tab.bars:
        logf = float(np.clip(logf + rng.normal(0, 0.03), np.log(0.8), np.log(1.25)))
        f.append(g * np.exp(logf))
    bpm = {}
    for b, _, _, segs in occ:
        bpm.setdefault(b, segs[0][2])
    for b, bar in enumerate(tab.bars):
        bar.tempo_changes = [(0, bpm.get(b, tab.initial_tempo) * f[b])]
    return tab


def detune(tab):
    for t in tab.tracks:
        if t.kind != "drums":
            t.tuning = [p + 1 for p in t.tuning]
    return tab


def cut_bars(tab, rng):
    """Removes 15-25% of the bars in the middle; returns the removed score
    interval (= audio interval, since these recordings follow the tab)."""
    occ = tab.occurrences()
    n = len(tab.bars)
    k = max(1, int(round(n * rng.uniform(0.15, 0.25))))
    start = rng.integers(max(1, n // 4), max(2, n - k - n // 4 + 1))
    span = (occ[start][1], occ[start + k - 1][2])
    del tab.bars[start:start + k]
    for t in tab.tracks:
        del t.bars[start:start + k]
    return tab, span


# ---------------------------------------------------------------- scoring

def onset_errors(ref, est, tol=0.3):
    import mir_eval

    if not ref or not est:
        return np.array([])
    ri, rp = _iv(ref)
    ei, ep = _iv(est)
    m = mir_eval.transcription.match_notes(ri, rp, ei, ep, onset_tolerance=tol, offset_ratio=None)
    return np.array([est[j][0] - ref[i][0] for i, j in m])


def _iv(notes):
    iv = np.array([[max(n[0], 0.0), max(n[1], max(n[0], 0.0) + 0.001)] for n in notes], float).reshape(-1, 2)
    return iv, 440.0 * 2 ** ((np.array([n[2] for n in notes], float) - 69) / 12)


def f1(ref, est, tol):
    import mir_eval

    if not ref or not est:
        return 0.0
    ri, rp = _iv(ref)
    ei, ep = _iv(est)
    return float(mir_eval.transcription.precision_recall_f1_overlap(ri, rp, ei, ep, onset_tolerance=tol,
                                                                    offset_ratio=None)[2])


def metrics(ref, est):
    err = onset_errors(ref, est)
    a = np.abs(err)
    return {"n_ref": len(ref), "n_est": len(est), "matched": int(len(err)),
            "med_ms": float(np.median(a) * 1000) if len(a) else None,
            "p90_ms": float(np.percentile(a, 90) * 1000) if len(a) else None,
            "bias_ms": float(np.median(err) * 1000) if len(err) else None,
            "f50": f1(ref, est, 0.05), "f100": f1(ref, est, 0.1), "f300": f1(ref, est, 0.3)}


# ---------------------------------------------------------------- runs

def features(audio_path, pad, cache):
    key = Path(cache) / "pad" / f"{Path(audio_path).parent.name}_{Path(audio_path).stem}_{pad:.2f}_{at.RATE}_{at.ONSET_OFFSET_MS:g}.pkl"
    if pad == 0:
        return at.cached_audio_features(audio_path, cache)
    if key.exists():
        import pickle

        return pickle.load(open(key, "rb"))
    import pickle

    x = at.load_audio(audio_path)
    rng = np.random.default_rng(int(pad * 1000))
    x = np.concatenate([rng.normal(0, 1e-4, int(pad * at.FS)), x])
    f = at.audio_features(x)
    key.parent.mkdir(parents=True, exist_ok=True)
    pickle.dump(f, open(key, "wb"))
    return f


def run_one(task):
    item, cond, tone, audio_path, other_tab, cache, seed = task
    rng = np.random.default_rng(seed)
    tab = gp_tabs.load(other_tab or item["tab"])
    ref = labels_of(item)
    pad, span = 0.0, None
    if cond in ("tempo", "mix"):
        tab = perturb_tempo(tab, rng)
        pad = float(np.round(rng.uniform(2, 6), 2))
    elif cond == "detune":
        tab = detune(tab)
    elif cond == "cut":
        tab, span = cut_bars(tab, rng)
    ref = [(s + pad, e + pad, p) for s, e, p in ref]
    if span is not None:
        ref = [n for n in ref if not (span[0] - 0.05 <= n[0] < span[1] + 0.05)]
    feats = features(audio_path, pad, cache)
    occ = tab.occurrences()
    t0 = time.time()
    al = at.align(at.alignment_notes(tab), occ[-1][2], feats)
    align_secs = time.time() - t0
    st = al["stats"]
    status, reasons = at.verdict(st, st["transpose_found"], 2, {"cost": at.MAX_COST, "stall": at.MAX_STALL})
    out = {"set": item["set"], "item": item["name"], "cond": cond, "tone": tone, "pad": pad,
           "wrong_tab": Path(other_tab).stem if other_tab else None, "stats": st, "transpose": al["transpose"],
           "status": status, "reasons": reasons, "variants": {}, "align_secs": align_secs,
           "audio_secs": feats["duration"]}
    if cond == "wrong":
        return out
    guitars = [i for i, t in enumerate(tab.tracks) if t.kind == "guitar"] or [0]

    def est_of():
        notes = []
        for ti in guitars:
            notes += at.aligned_track_notes(tab, ti, al)
        return sorted((n["start"], n["end"], n["pitch"]) for n in notes)

    out["variants"]["dtw"] = metrics(ref, est_of())
    if cond == "clean":
        raw = sorted((n["start"], n["end"], n["pitch"]) for ti in guitars for n in tab.notes(ti))
        out["variants"]["tab"] = metrics(ref, raw)
    return out


def tasks_for(items, args):
    mixes = mixes_of(args.mixes)
    tasks = []
    for k, it in enumerate(items):
        tones = list(it["audio"])
        main = "amp_2" if "amp_2" in it["audio"] else tones[0]
        clean_tones = tones if it["set"] == "goat-test" else [main] + (["di"] if "di" in tones and main != "di" else [])
        if args.tones:
            clean_tones = [t for t in clean_tones if t in args.tones.split(",")] or clean_tones[:1]
        for tone in clean_tones:
            tasks.append((it, "clean", tone, it["audio"][tone], None, str(args.cache), 1000 * k + 1))
        for cond in ("tempo", "detune", "cut"):
            if cond in args.conditions:
                seed = 1000 * k + {"tempo": 2, "detune": 3, "cut": 4}[cond]
                tasks.append((it, cond, main, it["audio"][main], None, str(args.cache), seed))
        if "mix" in args.conditions:
            mix = mixes.get(it["labels"][1])
            if mix:
                tasks.append((it, "mix", "mix", mix, None, str(args.cache), 1000 * k + 7))
        if "wrong" in args.conditions:
            others = [o for o in items if o["set"].split("-")[0] == it["set"].split("-")[0] and o is not it]
            random.Random(k).shuffle(others)
            for o in others[: args.wrong_per_item]:
                tasks.append((it, "wrong", main, it["audio"][main], o["tab"], str(args.cache), 1000 * k + 11))
    return tasks


def _warm(path):
    at.cached_audio_features(path, CACHE)
    return path


def _init(cache, rate, onset_offset):
    global CACHE
    CACHE = cache
    at.RATE, at.ONSET_OFFSET_MS = rate, onset_offset


# ---------------------------------------------------------------- report

def q(vals, p):
    vals = [v for v in vals if v is not None]
    return float(np.percentile(vals, p)) if vals else float("nan")


def summarize(results):
    lines = []
    sets = sorted({r["set"] for r in results})
    lines.append("## Accuracy (per run; med/p90 = median over runs of the per-run median / p90 |onset error|)\n")
    lines.append("| set | condition | variant | runs | med ms | p90 ms | bias ms | F1@50 | F1@100 | F1@300 | accepted |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|")
    for s in sets:
        for c in CONDITIONS[:-1]:
            rs = [r for r in results if r["set"] == s and r["cond"] == c]
            if not rs:
                continue
            for v in rs[0]["variants"]:
                ms = [r["variants"][v] for r in rs if v in r["variants"]]
                acc = sum(r["status"] == "accepted" for r in rs)
                lines.append(f"| {s} | {c} | {v} | {len(ms)} | {q([m['med_ms'] for m in ms], 50):.0f} | "
                             f"{q([m['p90_ms'] for m in ms], 50):.0f} | {q([m['bias_ms'] for m in ms], 50):+.0f} | "
                             f"{np.mean([m['f50'] for m in ms]):.3f} | {np.mean([m['f100'] for m in ms]):.3f} | "
                             f"{np.mean([m['f300'] for m in ms]):.3f} | {acc}/{len(rs)} |")
    lines.append("\n## Rejection statistics (quantiles 0/10/50/90/100 over runs)\n")
    lines.append("| set | condition | runs | accepted | cost_ratio | stall | worst_window (info) |")
    lines.append("|---|---|---|---|---|---|---|")
    for s in sets:
        for c in CONDITIONS:
            rs = [r for r in results if r["set"] == s and r["cond"] == c]
            if not rs:
                continue

            def qs(key):
                return "/".join(f"{q([r['stats'][key] for r in rs], p):.2f}" for p in (0, 10, 50, 90, 100))

            acc = sum(r["status"] == "accepted" for r in rs)
            lines.append(f"| {s} | {c} | {len(rs)} | {acc} | {qs('cost_ratio')} | {qs('stall')} | {qs('worst_window')} |")
    a = sum(r["audio_secs"] for r in results if "audio_secs" in r)
    w = sum(r["align_secs"] for r in results if "align_secs" in r)
    if w:
        lines.append(f"\nDTW + statistics: {a / w:.1f} s of audio per second (one core, features cached)")
    lines.append("\n## Transposition found (detune expects -1, others 0)\n")
    for c in ("clean", "tempo", "detune", "mix"):
        rs = [r for r in results if r["cond"] == c]
        if rs:
            counts = defaultdict(int)
            for r in rs:
                counts[r["stats"]["transpose_found"]] += 1
            lines.append(f"- {c}: " + ", ".join(f"{k:+d}: {v}" for k, v in sorted(counts.items())))
    return "\n".join(lines)


def per_item(results, variant):
    lines = [f"\n## Per item ({variant})\n", "| set | item | cond | tone | med ms | p90 ms | bias ms | F1@50 | F1@300 | cost | worst | stall | status |",
             "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in sorted(results, key=lambda r: (r["set"], r["item"], r["cond"], r["tone"])):
        if r["cond"] == "wrong" or variant not in r["variants"]:
            continue
        m, st = r["variants"][variant], r["stats"]
        fmt = lambda v, f: (f"{v:{f}}" if v is not None else "-")  # noqa: E731
        lines.append(f"| {r['set']} | {r['item']} | {r['cond']} | {r['tone']} | {fmt(m['med_ms'], '.0f')} | "
                     f"{fmt(m['p90_ms'], '.0f')} | {fmt(m['bias_ms'], '+.0f')} | {m['f50']:.3f} | {m['f300']:.3f} | "
                     f"{st['cost_ratio']:.2f} | {st['worst_window']:.2f} | {st['stall']:.2f} | {r['status']} |")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--egset12", type=Path)
    ap.add_argument("--goat", type=Path, help="GOAT folder with metadata.csv and data/")
    ap.add_argument("--goat-train", type=int, default=0, help="also use this many GOAT train items")
    ap.add_argument("--mixes", type=Path, help="make_mixes.py manifest whose notes column is the label file")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--cache", type=Path, default=None)
    ap.add_argument("--conditions", default=",".join(CONDITIONS))
    ap.add_argument("--wrong-per-item", type=int, default=6)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--report-only", action="store_true")
    ap.add_argument("--rate", type=int, default=at.RATE, help="feature rate (Hz)")
    ap.add_argument("--onset-offset", type=float, default=at.ONSET_OFFSET_MS, help="audio onset peak shift (ms)")
    ap.add_argument("--tones", default="", help="only these tones for clean runs (e.g. di,amp_2)")
    args = ap.parse_args()
    for k in ("egset12", "goat", "mixes", "out", "cache"):
        if getattr(args, k):
            setattr(args, k, getattr(args, k).expanduser())
    args.cache = args.cache or args.out / "cache"
    args.conditions = args.conditions.split(",")
    args.out.mkdir(parents=True, exist_ok=True)
    res_file = args.out / "results.jsonl"
    if not args.report_only:
        items = collect(args)
        tasks = tasks_for(items, args)
        print(f"{len(items)} items, {len(tasks)} runs", flush=True)
        audio = sorted({t[3] for t in tasks})
        with Pool(args.jobs, initializer=_init, initargs=(str(args.cache), args.rate, args.onset_offset)) as pool:
            for k, _ in enumerate(pool.imap_unordered(_warm, audio)):
                if (k + 1) % 10 == 0:
                    print(f"features {k + 1}/{len(audio)}", flush=True)
            with open(res_file, "w") as f:
                for k, r in enumerate(pool.imap_unordered(run_one, tasks)):
                    f.write(json.dumps(r) + "\n")
                    f.flush()
                    if (k + 1) % 20 == 0:
                        print(f"runs {k + 1}/{len(tasks)}", flush=True)
    results = [json.loads(l) for l in open(res_file)]
    text = summarize(results) + per_item(results, "dtw") + "\n"
    (args.out / "summary.md").write_text(text)
    print(text)


if __name__ == "__main__":
    main()
