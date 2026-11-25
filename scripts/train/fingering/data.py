"""Notes with strings for the fingering model, from every labelled set, into
one JSON file: `{"recordings": [{"id", "source", "split", "group",
"tuning", "notes": [[onset, offset, pitch, string], ...]}, ...]}`.

Splits (strict):
- train / val (10% of the recordings of each source, by a hash of the id):
  GuitarSet players 00-02, EGDB tabs with id % 5 != 0, Guitar-TECHS
  chords, scales and single notes (P1/P2; strings from the hexaphonic MIDI
  track names), GOAT's train split (Guitar Pro tabs; non-commercial, only
  the "full" model uses it).
- test, never used for fitting or selection: GuitarSet 03-05 (comp, solo),
  EGDB id % 5 == 0, EGSet12, GOAT's test split. GOAT test items also carry
  `aligned`: the fine-aligned MIDI notes with the tab's strings, for Tab F1
  against transcribed audio.

GuitarSet / EGSet12 / EGDB use `bench_tab.py`'s readers.

Usage: python data.py --root ~/.cache/gva_test --out ~/gva/fingering/data.json
"""
import argparse
import csv
import hashlib
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

import common  # noqa: F401  (puts bench_tab and prepare_data on the path)
from bench_tab import STANDARD, egdb_tab, guitarset_tab  # noqa: E402

MIN_NOTE = 0.03  # hexaphonic pickup blips


def val_split(rid):
    return "val" if int(hashlib.md5(rid.encode()).hexdigest(), 16) % 10 == 0 else "train"


def techs_tab(path):
    """Guitar-TECHS hexaphonic MIDI: one track per string, named e B G D A E."""
    import mido

    names = {"e": 1, "B": 2, "G": 3, "D": 4, "A": 5, "E": 6}
    m = mido.MidiFile(path)
    tempos = sorted((tick, msg.tempo) for t in m.tracks for tick, msg in _ticks(t) if msg.type == "set_tempo") or [(0, 500000)]
    if tempos[0][0] > 0:
        tempos.insert(0, (0, 500000))

    def secs(tick):
        t, last, tempo = 0.0, 0, tempos[0][1]
        for at, new in tempos[1:]:
            if at >= tick:
                break
            t += mido.tick2second(at - last, m.ticks_per_beat, tempo)
            last, tempo = at, new
        return t + mido.tick2second(tick - last, m.ticks_per_beat, tempo)

    out = []
    for track in m.tracks:
        s = names.get(track.name.strip())
        if s is None:
            continue
        on = {}
        for tick, msg in _ticks(track):
            if msg.type == "note_on" and msg.velocity > 0:
                on[msg.note] = tick
            elif msg.type in ("note_off", "note_on") and msg.note in on:
                a, b = secs(on.pop(msg.note)), secs(tick)
                if b - a >= MIN_NOTE:
                    out.append((a, b, msg.note, s))
    return sorted(out)


def _ticks(track):
    tick = 0
    for msg in track:
        tick += msg.time
        yield tick, msg


def goat_tab(gp5):
    """(tuning, [(onset, offset, pitch, string)]) of the first track of a
    Guitar Pro file, score time; tied and dead notes skipped."""
    import guitarpro

    song = guitarpro.parse(str(gp5))
    track = song.tracks[0]
    tuning = [s.value for s in sorted(track.strings, key=lambda s: s.number)]
    tempo, now, out = float(song.tempo), 0.0, []
    for measure in track.measures:
        start, length = now, 0.0
        for voice in measure.voices[:1]:
            t = start
            for beat in voice.beats:
                mix = beat.effect.mixTableChange
                if mix is not None and mix.tempo is not None and mix.tempo.value > 0:
                    tempo = float(mix.tempo.value)
                dur = beat.duration.time / 960 * 60 / tempo
                for n in beat.notes:
                    if n.type in (guitarpro.NoteType.tie, guitarpro.NoteType.dead):
                        continue
                    out.append((t, t + dur, tuning[n.string - 1] + n.value, n.string))
                t += dur
            length = max(length, t - start)
        now = start + length
    return tuning, sorted(out)


def goat_aligned(d, item, tab):
    """Fine-aligned MIDI notes with the tab's strings: each aligned note takes
    the string of the nearest tab note of the same pitch within 0.3 s (tab
    times are the unaligned MIDI's score times), after the constant pitch
    shift (-12..12) that matches most notes (some tabs are written in another
    tuning than the audio). Unmatched notes get string 0 (no label)."""
    from prepare_data import midi_notes

    aligned = midi_notes(d / f"{item}_fine_aligned.mid")

    def match(shift):
        by_pitch = defaultdict(list)
        for t in tab:
            by_pitch[t[2] + shift].append(t)
        out = []
        for on, off, p, _ in aligned:
            near = [x for x in by_pitch[p] if abs(x[0] - on) < 0.3]
            out.append((on, off, p, min(near, key=lambda x: abs(x[0] - on))[3] if near else 0))
        return out

    return max((match(k) for k in range(-12, 13)), key=lambda o: sum(x[3] > 0 for x in o))


def rec(rid, source, split, group, tuning, notes, **extra):
    return {"id": rid, "source": source, "split": split, "group": group, "tuning": list(tuning),
            "notes": [[round(a, 4), round(b, 4), int(p), int(s)] for a, b, p, s in notes], **extra}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    root = args.root.expanduser()
    out = []
    # GuitarSet: 00-02 train/val, 03-05 test.
    for jams in sorted((root / "guitarset/annotation").glob("*.jams")):
        player = jams.stem[:2]
        kind = "comp" if jams.stem.endswith("_comp") else "solo"
        rid = f"guitarset/{jams.stem}"
        split = val_split(rid) if player in ("00", "01", "02") else "test"
        out.append(rec(rid, f"guitarset_{kind}", split, f"guitarset/{kind}", STANDARD, guitarset_tab(jams)))
    # EGDB.
    for midi in sorted((root / "egdb/audio_label").glob("*.midi"), key=lambda p: int(p.stem)):
        i = int(midi.stem)
        rid = f"egdb/{i}"
        out.append(rec(rid, "egdb", "test" if i % 5 == 0 else val_split(rid), "egdb", STANDARD, egdb_tab(midi)))
    # EGSet12: test only.
    for jams in sorted((root / "egset12").glob("*.jams")):
        out.append(rec(f"egset12/{jams.stem}", "egset12", "test", "egset12", STANDARD, guitarset_tab(jams)))
    # Guitar-TECHS training parts.
    for part in sorted((root / "guitar_techs").glob("P[12]_*")):
        kind = part.name.split("_", 1)[1]
        if kind not in ("chords", "scales", "singlenotes") or not part.is_dir():
            continue
        for mid in sorted((part / "midi").glob("*.mid")):
            rid = f"techs/{part.name}/{mid.stem}"
            out.append(rec(rid, "techs", val_split(rid), f"techs/{kind}", STANDARD, techs_tab(mid)))
    # GOAT.
    goat = root / "goat/GOAT"
    if (goat / "metadata.csv").exists():
        for row in csv.DictReader(open(goat / "metadata.csv")):
            item = row["item"]
            d = goat / "data" / item
            try:
                tuning, notes = goat_tab(d / f"{item}.gp5")
            except Exception as err:
                print(f"{item}: unreadable tab ({err})", file=sys.stderr)
                continue
            if len(tuning) != 6:
                print(f"{item}: {len(tuning)} strings, skipped", file=sys.stderr)
                continue
            rid = f"goat/{item}"
            if row["split"] == "test":
                extra = {"aligned": [[round(a, 4), round(b, 4), int(p), int(s)] for a, b, p, s in goat_aligned(d, item, notes)]}
                out.append(rec(rid, "goat", "test", "goat", tuning, notes, **extra))
            else:
                out.append(rec(rid, "goat", val_split(rid), "goat", tuning, notes))
    # Sanity: notes whose pitch can't be played on their string.
    bad = Counter()
    for r in out:
        for a, b, p, s in r["notes"]:
            f = p - r["tuning"][s - 1] if 1 <= s <= 6 else -1
            if not 0 <= f <= 24:
                bad[r["source"]] += 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    json.dump({"recordings": out}, open(args.out, "w"))
    counts = defaultdict(lambda: [0, 0])
    for r in out:
        c = counts[(r["split"], r["group"])]
        c[0] += 1
        c[1] += len(r["notes"])
    print("| split | group | recordings | notes |\n|---|---|---|---|")
    for (split, g), (n, m) in sorted(counts.items()):
        print(f"| {split} | {g} | {n} | {m} |")
    print("unplayable labels per source:", dict(bad))
    tunings = Counter(tuple(r["tuning"]) for r in out)
    print("tunings:", dict(tunings))


if __name__ == "__main__":
    main()
