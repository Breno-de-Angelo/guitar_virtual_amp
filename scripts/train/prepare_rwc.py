"""Guitar note labels for the RWC Popular Music Database (RWC-P, 100 real
band recordings, CC BY-NC 4.0; `fetch_rwc.py` downloads them): the
community-aligned MIDI files (RWC 2.0, re-aligned with DTW and checked by
hand) contain each song's guitar parts, transcribed by karaoke-industry
musicians after the recording. They are a human reference for the guitar
inside a real mix, without strings: note F50 and octave errors, not Tab F1.

Guitar notes are the notes on channels whose current General MIDI program
is a guitar (24-31), between MIDI 36 and 96 and at least 20 ms long:
shorter notes are muted strokes and pitches below 36 are effect notes
(fret noise, key switches) in these files, not playable guitar notes (2.6%
and 8.5% of the guitar notes). Each song is labelled `distorted` when most guitar
notes use the overdrive / distortion programs (29, 30), `acoustic` when
most use the nylon / steel programs (24, 25), else `clean`. Unisons of
two guitar tracks (same pitch within 30 ms) are kept once.

Writes `<rwc>/notes/guitar/<id>.txt` (onset offset pitch velocity), one file
per guitar track in `<rwc>/notes/tracks/`, and the manifests
`<rwc>/eval_rwc.tsv` (full mix) and `<rwc>/rwc_tracks.tsv` (track list),
for `bench_separation.py`, `bench_notes.py` and the Rust `eval_manifest`.

Usage: python prepare_rwc.py --rwc ~/.cache/gva_test/rwc
"""
import argparse
from collections import Counter, defaultdict
from pathlib import Path

import mido

GUITAR = range(24, 32)
MIN_PITCH, MAX_PITCH, MIN_DUR = 36, 96, 0.02
DISTORTED, ACOUSTIC = {29, 30}, {24, 25}


def notes_by_track(path):
    """{(track index, track name): [(onset, offset, pitch, velocity, program)]}
    in seconds, programs per channel as they change."""
    m = mido.MidiFile(path)
    tempo_map = []  # (tick, tempo)
    for tr in m.tracks:
        tick = 0
        for msg in tr:
            tick += msg.time
            if msg.type == "set_tempo":
                tempo_map.append((tick, msg.tempo))
    tempo_map.sort()
    if not tempo_map or tempo_map[0][0] > 0:
        tempo_map.insert(0, (0, 500000))

    def secs(tick):
        t, last, tempo = 0.0, 0, tempo_map[0][1]
        for at, new in tempo_map[1:]:
            if at >= tick:
                break
            t += mido.tick2second(at - last, m.ticks_per_beat, tempo)
            last, tempo = at, new
        return t + mido.tick2second(tick - last, m.ticks_per_beat, tempo)

    # Programs can be set on one track and used on another: collect them first.
    changes = defaultdict(list)  # channel -> [(tick, program)]
    for tr in m.tracks:
        tick = 0
        for msg in tr:
            tick += msg.time
            if msg.type == "program_change":
                changes[msg.channel].append((tick, msg.program))
    for c in changes.values():
        c.sort()

    def program(channel, tick):
        p = 0
        for at, prog in changes.get(channel, []):
            if at > tick:
                break
            p = prog
        return p

    out = {}
    for k, tr in enumerate(m.tracks):
        tick, on, notes = 0, {}, []
        for msg in tr:
            tick += msg.time
            if msg.type not in ("note_on", "note_off") or msg.channel == 9:
                continue
            key = (msg.channel, msg.note)
            if msg.type == "note_on" and msg.velocity > 0:
                if key in on:  # retrigger: close the sounding note
                    t0, v = on.pop(key)
                    notes.append((t0, tick, msg.note, v, program(msg.channel, t0)))
                on[key] = (tick, msg.velocity)
            elif key in on:
                t0, v = on.pop(key)
                notes.append((t0, tick, msg.note, v, program(msg.channel, t0)))
        if notes:
            out[(k, tr.name.strip())] = sorted((secs(a), secs(b), p, v, prog) for a, b, p, v, prog in notes)
    return out


def dedupe(notes, window=0.03):
    """Drops a note when the same pitch already starts within `window`."""
    kept, last = [], {}
    for n in sorted(notes):
        t = last.get(n[2])
        if t is not None and n[0] - t < window:
            continue
        kept.append(n)
        last[n[2]] = n[0]
    return kept


def write(path, notes):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for on, off, p, v, *_ in notes:
            f.write(f"{on:.4f} {max(off, on + 0.01):.4f} {int(p)} {int(v)}\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rwc", type=Path, required=True)
    ap.add_argument("--min-notes", type=int, default=50, help="songs with fewer guitar notes are skipped")
    args = ap.parse_args()
    root = args.rwc.expanduser()
    rows, tracks, kinds = [], [], Counter()
    for midi in sorted((root / "midi").glob("RWC_P*.mid")):
        rid = midi.stem
        audio = root / "audio" / f"{rid}.wav"
        if not audio.exists():
            print(f"{rid}: no audio yet, skipped")
            continue
        per = {k: [n for n in v if n[4] in GUITAR and MIN_PITCH <= n[2] <= MAX_PITCH and n[1] - n[0] >= MIN_DUR]
               for k, v in notes_by_track(midi).items()}
        per = {k: v for k, v in per.items() if v}
        allg = dedupe([n for v in per.values() for n in v])
        if len(allg) < args.min_notes:
            continue
        progs = Counter(n[4] for n in allg)
        dist = sum(progs[p] for p in DISTORTED)
        acou = sum(progs[p] for p in ACOUSTIC)
        kind = "distorted" if dist > len(allg) / 2 else "acoustic" if acou > len(allg) / 2 else "clean"
        kinds[kind] += 1
        write(root / "notes/guitar" / f"{rid}.txt", allg)
        rows.append(f"{audio}\t{root / 'notes/guitar' / f'{rid}.txt'}\trwc/{kind}\t")
        for (k, name), v in sorted(per.items()):
            f = root / "notes/tracks" / f"{rid}__{k:02d}.txt"
            write(f, v)
            tracks.append(f"{rid}\t{k}\t{name}\t{Counter(n[4] for n in v).most_common(1)[0][0]}\t{len(v)}\t{f}")
    (root / "eval_rwc.tsv").write_text("\n".join(rows) + "\n")
    (root / "rwc_tracks.tsv").write_text("id\ttrack\tname\tprogram\tnotes\tfile\n" + "\n".join(tracks) + "\n")
    print(f"{len(rows)} songs ({dict(kinds)}), {len(tracks)} guitar tracks -> {root / 'eval_rwc.tsv'}")


if __name__ == "__main__":
    main()
