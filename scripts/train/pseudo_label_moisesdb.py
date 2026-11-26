"""Separation-aware training data from MoisesDB (roadmap T1): the network
hears a separator's guitar stem of the real mix, and learns the notes a
teacher model transcribed from the same song's isolated guitars.

Needs `prepare_moisesdb.py`, both separators run on `moises_mix.tsv`
(`bench_separation.py --manifest moises_mix.tsv`), and the teacher's notes on
`moises_iso.tsv` (`bench_notes.py run`), all in BENCH/notes/<teacher>/.

Splits songs by guitar kind (distorted / clean / acoustic) into test (25%),
val (8%) and train, deterministically (sha1 of the song id). Songs where
`--keep-test` has notes on every input (the MuScriptor subset) are always
test. Writes into DATA:
- `split.json`: song ids per split;
- `pseudo/<teacher>/<song>.txt`: teacher notes (onset offset pitch 0; no
  velocity, so `finetune_crnn.py` masks the velocity loss);
- `train_moises.jsonl`: `finetune_crnn.py --extra-train` items, variants =
  BS-RoFormer stem, htdemucs_6s stem, isolated guitars (the last keeps the
  clean behaviour: there it is plain self-distillation);
- `val_moises.tsv`: BS-RoFormer stems of the val songs with teacher notes,
  for `finetune_crnn.py --extra-val`;
- `test_songs.txt`: for `bench_moisesdb.py --songs`.

Usage:
  python pseudo_label_moisesdb.py --data ~/.cache/gva_test/prepared_moises --bench ~/gva/bench_moises \\
      --teacher crnn-v2_spark_s5000 [--keep-test muscriptor-large-known]
"""
import argparse
import hashlib
import json
from pathlib import Path

TEST, VAL = 0.25, 0.08


def song_of(path):
    return Path(path).stem.split("__")[-2]


def manifest(path):
    """song -> (audio, group) of a moises manifest."""
    out = {}
    for line in open(path):
        audio, _, group, *_ = line.rstrip("\n").split("\t")
        out[song_of(audio)] = (audio, group)
    return out


def note_file(bench, engine, group, audio):
    stem = Path(audio).stem
    # bench_notes.py item ids: "<group with / as _>__<audio stem>", sanitized.
    import re

    return bench / "notes" / engine / (re.sub(r"[^A-Za-z0-9_.-]", "_", f"{group}__{stem}") + ".json")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--bench", type=Path, required=True)
    ap.add_argument("--teacher", default="crnn-v2_spark_s5000")
    ap.add_argument("--keep-test", default="muscriptor-large-known")
    args = ap.parse_args()
    data, bench = args.data.expanduser(), args.bench.expanduser()
    iso = manifest(data / "moises_iso.tsv")
    inputs = {"mix": manifest(data / "moises_mix.tsv"),
              "bsrofo": manifest(data / "moises_mix_bsrofo_sw.tsv"),
              "htdemucs": manifest(data / "moises_mix_htdemucs_6s.tsv")}
    kind = {s: g.rsplit("/", 1)[1] for s, (_, g) in iso.items()}

    forced = set()
    if args.keep_test:
        for s in iso:
            have = [note_file(bench, args.keep_test, *iso[s][::-1]).exists()]
            for name in ("mix", "bsrofo"):
                a, g = inputs[name][s]
                have.append(note_file(bench, args.keep_test, g, a).exists())
            if all(have):
                forced.add(s)
    split = {"train": [], "val": [], "test": []}
    for k in sorted(set(kind.values())):
        songs = sorted((s for s in iso if kind[s] == k), key=lambda s: hashlib.sha1(s.encode()).hexdigest())
        n_test, n_val = round(TEST * len(songs)), round(VAL * len(songs))
        test = [s for s in songs if s in forced]
        rest = [s for s in songs if s not in forced]
        while len(test) < n_test and rest:
            test.append(rest.pop(0))
        split["test"] += test
        split["val"] += rest[:n_val]
        split["train"] += rest[n_val:]
    json.dump(split, open(data / "split.json", "w"), indent=1)
    (data / "test_songs.txt").write_text("\n".join(sorted(split["test"])) + "\n")

    labels = data / "pseudo" / args.teacher
    labels.mkdir(parents=True, exist_ok=True)
    train, val, missing = [], [], 0
    for s in split["train"] + split["val"]:
        f = note_file(bench, args.teacher, iso[s][1], iso[s][0])
        if not f.exists():
            missing += 1
            continue
        notes = [n for n in json.load(open(f)) if "guitar" in str(n[3])]
        txt = labels / f"{s}.txt"
        txt.write_text("".join(f"{a:.4f} {b:.4f} {p} 0\n" for a, b, p, *_ in notes))
        if s in split["val"]:
            val.append(f"{inputs['bsrofo'][s][0]}\t{txt}\tmoises-val/{kind[s]}")
        else:
            audio = [inputs["bsrofo"][s][0], inputs["htdemucs"][s][0], iso[s][0]]
            train.append(json.dumps({"audio": audio, "notes": str(txt), "source": f"moises/{kind[s]}"}))
    (data / "train_moises.jsonl").write_text("\n".join(train) + "\n")
    (data / "val_moises.tsv").write_text("\n".join(val) + "\n")
    count = {k: {kk: sum(kind[s] == kk for s in v) for kk in ("distorted", "clean", "acoustic")} for k, v in split.items()}
    print(f"split {count}; {len(forced)} songs forced into test; {len(train)} train items, {len(val)} val; "
          f"{missing} songs without teacher notes")


if __name__ == "__main__":
    main()
