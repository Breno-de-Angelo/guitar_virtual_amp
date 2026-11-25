"""Fingering benchmark: the learned model (ONNX, onnxruntime) next to the
app's Viterbi and the lowest-fret baseline, with `bench_tab.py`'s readers,
item lists and scoring (imported, not modified).

- String accuracy on the true notes (share of notes on the reference string,
  mean per item, as `bench_tab.py`), per test set: GuitarSet 03-05 comp and
  solo, EGDB test, EGSet12, GOAT test (tab notes, from `data.py`'s JSON).
- Tab F1 with transcribed notes (`bench_notes.py` cache, `--notes-engine`):
  "<engine> + viterbi" against "<engine> + <model>", every audio group
  (EGDB tones, mixes and stems, GuitarSet, EGSet12, GOAT DI / amps against
  the fine-aligned MIDI with tab strings).
- Playability on the true notes: chords (onsets within 50 ms, >= 2 notes)
  with a stretch > 5 frets between fretted notes, or two notes on a string.

`--model name=path.onnx` can repeat; several paths for one name
(`name=a.onnx,b.onnx`) are seeds, reported as mean +- std. `--limit 12`
gives bench_tab.py's default item lists; `--limit 0` every test item (Tab F1
only where the notes cache has the item).

Usage (laptop, the Viterbi binary is x86):
  python bench.py --data ~/.cache/gva_test/prepared --guitarset ~/.cache/gva_test/guitarset \\
      --egdb ~/.cache/gva_test/egdb --egset12 ~/.cache/gva_test/egset12 --bench ~/.cache/gva_test/bench \\
      --fingering-bin target/release/examples/fingering --fing-data data.json \\
      --model ship=runs/ship_s1/model.onnx,runs/ship_s2/model.onnx --limit 0 --out results.json
"""
import argparse
import json
import subprocess
from collections import defaultdict
from pathlib import Path

import numpy as np

from common import STANDARD, candidate_frets, decode, features, playability, sort_notes, windowed
from bench_tab import lowest, scores, sets, truth  # noqa: E402
from bench_notes import spread  # noqa: E402


class Model:
    def __init__(self, path):
        import onnxruntime as ort

        so = ort.SessionOptions()
        so.intra_op_num_threads = 2
        self.sess = ort.InferenceSession(str(path), so, providers=["CPUExecutionProvider"])
        info = json.load(open(str(path) + ".json"))
        self.decode = info.get("decode") or {"span_cost": 0.5, "move_cost": 0.0}

    def run(self, f, c):
        return self.sess.run(None, {"frets": f, "cont": c})[0]

    def place(self, notes, tuning=STANDARD, raw=False):
        """[(string, fret)] per note of `notes` [(onset, offset, pitch, ...)]."""
        if not notes:
            return []
        order = sort_notes(notes)
        sn = [notes[i] for i in order]
        frets, cont, cid = features(sn, tuning)
        lg = windowed(self.run, frets, cont)
        if raw:
            s = np.argmax(np.where(frets != 25, lg, -1e9), 1)
        else:
            s = decode(lg, frets, cid, **self.decode)
        out = [None] * len(notes)
        for k, i in enumerate(order):
            out[i] = (int(s[k]) + 1, int(frets[k, s[k]]))
        return out


def viterbi(binary, cases):
    """`cases`: [(tuning, [(onset, pitch)])] -> [(string, fret)] lists (the
    app's `fret_events_with`, through `examples/fingering.rs`)."""
    lines = "\n".join(json.dumps({"tuning": list(t), "notes": [[float(a), int(p)] for a, p in c]}) for t, c in cases)
    res = subprocess.run([str(binary)], input=lines + "\n", capture_output=True, text=True, check=True)
    return [[tuple(x) for x in json.loads(l)] for l in res.stdout.splitlines()]


def items_all(args, recs):
    """True-note test items: bench_tab's lists plus GOAT test tabs."""
    its = sets(args.data, args.guitarset, args.egdb, args.limit, args.egset12)
    out = []
    for it in its:
        g = it["group"]
        if g in ("guitarset/comp", "guitarset/solo", "egset12"):
            out.append((g, it, truth(it), STANDARD))
        elif g == "egdb/DI":  # the other tones have the same labels
            out.append(("egdb", it, truth(it), STANDARD))
    goat = [r for r in recs if r["source"] == "goat" and r["split"] == "test"]
    for r in spread(goat, args.limit):
        out.append(("goat", {"id": r["id"], "group": "goat"}, [tuple(x) for x in r["notes"]], r["tuning"]))
    return out


def audio_items(args, recs):
    """(group, id, truth with strings, tuning) for every cached transcription."""
    out = []
    for it in sets(args.data, args.guitarset, args.egdb, args.limit, args.egset12):
        out.append((it["group"], it["id"], truth(it), STANDARD))
    goat = {r["id"].split("/")[1]: r for r in recs if r["source"] == "goat" and r["split"] == "test"}
    for item, r in sorted(goat.items()):
        ref = [tuple(x) for x in r["aligned"]]
        for tone in ["di"] + [f"amp_{k}" for k in range(1, 6)]:
            stem = item if tone == "di" else f"{item}_{tone}"
            out.append((f"goat/{tone}", f"goat_{tone}__{stem}", ref, r["tuning"]))
    return out


def mean_std(vals):
    vals = [v for v in vals if v is not None]
    if not vals:
        return None
    return float(np.mean(vals)), float(np.std(vals)), len(vals)


def fmt(x):
    if x is None:
        return ""
    m, s, n = x
    return f"{m:.3f}" if n == 1 else f"{m:.3f} ± {s:.3f}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True, help="prepared folder with eval_egdb.tsv (bench_tab --data)")
    ap.add_argument("--guitarset", type=Path, required=True)
    ap.add_argument("--egdb", type=Path, required=True)
    ap.add_argument("--egset12", type=Path, default=None)
    ap.add_argument("--bench", type=Path, required=True)
    ap.add_argument("--notes-engine", default="crnn-v2_spark_s5000")
    ap.add_argument("--fingering-bin", type=Path, required=True)
    ap.add_argument("--fing-data", type=Path, required=True, help="data.py output (GOAT test tabs)")
    ap.add_argument("--model", action="append", default=[], help="name=a.onnx[,b.onnx...]")
    ap.add_argument("--raw", action="store_true", help="also score the models' argmax without decoding")
    ap.add_argument("--limit", type=int, default=12)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    for k in ("data", "guitarset", "egdb", "bench", "fingering_bin", "fing_data"):
        setattr(args, k, getattr(args, k).expanduser())
    args.egset12 = args.egset12.expanduser() if args.egset12 else None
    recs = json.load(open(args.fing_data))["recordings"]
    models = {}
    for spec in args.model:
        name, paths = spec.split("=", 1)
        models[name] = [Model(p) for p in paths.split(",")]

    # ------------------------------------------------ true notes
    items = items_all(args, recs)
    vit = viterbi(args.fingering_bin, [(tun, [(n[0], n[2]) for n in t]) for _, _, t, tun in items])
    acc = defaultdict(lambda: defaultdict(list))  # method -> group -> [per-seed list of per-item acc]
    play = defaultdict(lambda: defaultdict(lambda: np.zeros(3)))

    def record(method, g, t, placed, seed=0):
        a = float(np.mean([s == n[3] for (s, _), n in zip(placed, t)])) if t else 0.0
        while len(acc[method][g]) <= seed:
            acc[method][g].append([])
        acc[method][g][seed].append(a)
        if seed == 0:
            order = sort_notes(t)
            play[method][g] += playability([t[i] for i in order], [placed[i][0] for i in order], [placed[i][1] for i in order])

    for (g, it, t, tun), v in zip(items, vit):
        record("viterbi (app)", g, t, v)
        record("lowest fret", g, t, lowest([(n[0], n[2]) for n in t]))
        fr = candidate_frets([n[2] for n in t], tun)
        record("reference", g, t, [(n[3], int(fr[i, n[3] - 1]) if 1 <= n[3] <= 6 and fr[i, n[3] - 1] != 25 else -1) for i, n in enumerate(t)])
        for name, ms in models.items():
            for k, m in enumerate(ms):
                record(name, g, t, m.place(t, tun), k)
                if args.raw:
                    record(f"{name} (argmax)", g, t, m.place(t, tun, raw=True), k)
    groups = ["guitarset/comp", "guitarset/solo", "egdb", "egset12", "goat"]
    report = {"limit": args.limit, "string_acc": {}, "playability": {}, "tab_f1": {}, "f50": {}, "n_items": {}}
    for method, per in acc.items():
        report["string_acc"][method] = {g: mean_std([float(np.mean(x)) for x in per[g]]) for g in groups if g in per}
    for g in groups:
        report["n_items"][g] = len(acc["viterbi (app)"].get(g, [[]])[0])
    for method, per in play.items():
        report["playability"][method] = {g: {"chords": int(c[0]), "stretch": float(c[1] / max(1, c[0])), "same_string": float(c[2] / max(1, c[0]))} for g, c in per.items()}

    # ------------------------------------------------ transcribed notes
    eng = args.notes_engine
    cases = []
    for g, iid, ref, tun in audio_items(args, recs):
        f = args.bench / "notes" / eng / f"{iid}.json"
        if f.exists():
            est = [(n[0], n[1], n[2]) for n in json.load(open(f)) if len(n) < 4 or "guitar" in n[3]]
            cases.append((g, iid, ref, tun, est))
    vit = viterbi(args.fingering_bin, [(tun, [(n[0], n[2]) for n in est]) for _, _, _, tun, est in cases])
    tab = defaultdict(lambda: defaultdict(list))
    f50 = {}
    for (g, iid, ref, tun, est), v in zip(cases, vit):
        sc = scores(ref, [(a, b, p, s) for (a, b, p), (s, _) in zip(est, v) if s > 0])
        tab[f"{eng} + viterbi"][g].append([sc["tab_f1"]])
        f50.setdefault(g, []).append(sc["f50"])
        for name, ms in models.items():
            vals = []
            for m in ms:
                pl = m.place(est, tun)
                vals.append(scores(ref, [(a, b, p, s) for (a, b, p), (s, _) in zip(est, pl)])["tab_f1"])
            tab[f"{eng} + {name}"][g].append(vals)
    agroups = sorted(f50)
    for method, per in tab.items():
        # per[g]: one [seed values] list per item -> mean over items, per seed
        report["tab_f1"][method] = {}
        for g in agroups:
            rows = [r for r in per.get(g, []) if r]
            if rows:
                k = min(len(r) for r in rows)
                report["tab_f1"][method][g] = mean_std([float(np.mean([r[s] for r in rows])) for s in range(k)])
    report["f50"] = {g: float(np.mean(v)) for g, v in f50.items()}
    report["tab_items"] = {g: len(v) for g, v in f50.items()}
    if args.out:
        json.dump(report, open(args.out, "w"), indent=1)

    print(f"## String accuracy on true notes (limit {args.limit}; items: " + ", ".join(f"{g} {report['n_items'][g]}" for g in groups) + ")")
    print("| method | " + " | ".join(groups) + " |\n|---|" + "---|" * len(groups))
    for method, row in report["string_acc"].items():
        if method != "reference":
            print(f"| {method} | " + " | ".join(fmt(row.get(g)) for g in groups) + " |")
    print("\n## Playability on true notes: chords with stretch > 5 frets / two notes on a string")
    print("| method | " + " | ".join(groups) + " |\n|---|" + "---|" * len(groups))
    for method, row in report["playability"].items():
        print(f"| {method} | " + " | ".join(f"{row[g]['stretch']:.3f} / {row[g]['same_string']:.3f}" if g in row else "" for g in groups) + " |")
    print(f"\n## Tab F1 with {eng} notes (F50 of the notes: " + ", ".join(f"{g} {v:.2f}" for g, v in report["f50"].items()) + ")")
    print("| method | " + " | ".join(agroups) + " |\n|---|" + "---|" * len(agroups))
    for method, row in report["tab_f1"].items():
        print(f"| {method} | " + " | ".join(fmt(row.get(g)) for g in agroups) + " |")


if __name__ == "__main__":
    main()
