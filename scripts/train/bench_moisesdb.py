"""Scores note models on real mixes from MoisesDB (`prepare_moisesdb.py`),
where there are no note labels: every model's guitar notes on the mix, or on
a separator's guitar stem, are compared with notes transcribed from the same
song's isolated guitar tracks.

References (per song, from the notes cached by `bench_notes.py run` on
`moises_iso.tsv`):
- `self`: the same model on the isolated guitars. Measures only what the mix
  (or the separator) costs that model; a model that is wrong on clean guitar
  can still score well.
- `consensus`: notes that every `--ref` engine found on the isolated guitars
  (mir_eval match, onset 50 ms + pitch; the first engine's timing). No single
  model's habits decide the reference, at the price of dropping notes the
  engines disagree on, so it leans towards easy notes.
The `iso` row against `consensus` shows how each model does on the isolated
guitars themselves.

Inputs: `mix` (`moises_mix.tsv`) and every `moises_mix_<separator>.tsv`
written by `bench_separation.py --manifest moises_mix.tsv`.

Usage:
  python bench_moisesdb.py --data PREPARED_MOISES --bench BENCH \\
      [--ref crnn-v2_spark_s5000,muscriptor-large-known] [--common ENGINE] [--out report.json]
`--common ENGINE` keeps only songs where ENGINE has notes for every input,
so engines run on a subset (MuScriptor) are compared on the same songs.
"""
import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench_notes import intervals, items, metrics  # noqa: E402


def guitar_notes(path):
    if not path.exists():
        return None
    return [n[:3] for n in json.load(open(path)) if "guitar" in str(n[3])]


def consensus(refs):
    """Notes of refs[0] matched (onset 50 ms, pitch) in every other list."""
    import mir_eval

    base = refs[0]
    keep = set(range(len(base)))
    if not base:
        return []
    bi, bp = intervals(base)
    for other in refs[1:]:
        if not other:
            return []
        oi, op = intervals(other)
        pairs = mir_eval.transcription.match_notes(bi, bp, oi, op, onset_tolerance=0.05, offset_ratio=None)
        keep &= {i for i, _ in pairs}
    return [base[i] for i in sorted(keep)]


def song_of(it):
    """MoisesDB song id: `<song>__mix`, `<song>__guitar`, or a separator's
    `<group>__<song>__mix`."""
    return Path(it["audio"]).stem.split("__")[-2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--bench", type=Path, required=True)
    ap.add_argument("--ref", default="crnn-v2_spark_s5000,muscriptor-large-known")
    ap.add_argument("--songs", type=Path, default=None, help="only the song ids listed in this file (e.g. test_songs.txt)")
    ap.add_argument("--common", default=None, help="only songs where this engine has notes for every input")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    data, bench = args.data.expanduser(), args.bench.expanduser()
    ref_engines = args.ref.split(",")
    notes_dir = bench / "notes"
    engines = sorted(p.name for p in notes_dir.iterdir() if p.is_dir())

    iso = {song_of(it): it for it in items(data, None, 0, ["moises_iso.tsv"])}
    inputs = {"iso": iso}
    for m in sorted(data.glob("moises_mix*.tsv")):
        name = "mix" if m.stem == "moises_mix" else m.stem.removeprefix("moises_mix_")
        inputs[name] = {song_of(it): it for it in items(data, None, 0, [m.name])}

    def notes(engine, it):
        return guitar_notes(notes_dir / engine / f"{it['id']}.json")

    if args.songs:
        wanted = set(args.songs.expanduser().read_text().split())
        iso = {k: v for k, v in iso.items() if k in wanted}
        inputs = {name: {k: v for k, v in its.items() if k in wanted} for name, its in inputs.items()}
    if args.common:
        keep = {song for song in iso
                if all(song in its and notes(args.common, its[song]) is not None for its in inputs.values()
                       if any(notes(args.common, it) is not None for it in its.values()))}
        iso = {k: v for k, v in iso.items() if k in keep}
        inputs = {name: {k: v for k, v in its.items() if k in keep} for name, its in inputs.items()}
        print(f"--common {args.common}: {len(keep)} songs")

    refs = {}
    for song, it in iso.items():
        r = [notes(e, it) for e in ref_engines]
        if all(x is not None for x in r):
            refs[song] = consensus(r)
    print(f"{len(iso)} songs; consensus reference ({' & '.join(ref_engines)}) for {len(refs)}, "
          f"{np.mean([len(v) for v in refs.values()]) if refs else 0:.0f} notes per song on average")

    # rows[(engine, input, reference)][kind] -> list of metrics
    rows = defaultdict(lambda: defaultdict(list))
    for engine in engines:
        for inp, its in inputs.items():
            for song, it in its.items():
                est = notes(engine, it)
                if est is None:
                    continue
                kind = it["group"].rsplit("/", 1)[1]
                own = notes(engine, iso[song]) if song in iso else None
                if inp != "iso" and own:
                    m = metrics(own, est)
                    if m:
                        rows[(engine, inp, "self")][kind].append(m)
                if refs.get(song):
                    m = metrics(refs[song], est)
                    if m:
                        rows[(engine, inp, "consensus")][kind].append(m)
    kinds = ["distorted", "clean", "acoustic"]
    report = {}
    for key, per in sorted(rows.items()):
        label = " | ".join(key)
        allm = [m for k in per for m in per[k]]
        report[label] = {k: {"f50": float(np.mean([m["f50"] for m in per[k]])), "n": len(per[k])} for k in per}
        report[label]["all"] = {"f50": float(np.mean([m["f50"] for m in allm])), "n": len(allm),
                                "oct_low": float(np.mean([m["oct_low"] for m in allm]))}
    for ref in ("consensus", "self"):
        print(f"\n## F50 against the {ref} reference (mean per song; n songs)")
        print("| engine | input | " + " | ".join(kinds) + " | all | octave low (all) |")
        print("|---|---|" + "---|" * (len(kinds) + 2))
        for label, r in report.items():
            engine, inp, rr = label.split(" | ")
            if rr != ref:
                continue
            cells = [f"{r[k]['f50']:.3f} ({r[k]['n']})" if k in r else "" for k in kinds]
            print(f"| {engine} | {inp} | " + " | ".join(cells) + f" | {r['all']['f50']:.3f} ({r['all']['n']}) | {r['all']['oct_low']:.3f} |")
    if args.out:
        json.dump({"ref": ref_engines, "rows": report}, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
