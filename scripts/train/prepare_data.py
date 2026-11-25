"""Turns guitar transcription datasets into what the training script and the
Rust benchmark (`eval_manifest` in guitar_core/src/transcribe/eval.rs) read:

- `<out>/notes/<dataset>/<name>.txt`: one note per line, `onset offset pitch
  velocity [technique]` (seconds, MIDI pitch, 0-127 or 0 when unknown, and
  for IDMT `none`, `bend`, `slide`, `vibrato`, `harmonic` or `dead`);
- `<out>/eval_<set>.tsv`: `audio <TAB> notes <TAB> group [<TAB> technique]`;
- `<out>/train.jsonl`: one recording per line, `{"audio": [variants...],
  "notes": "<notes file>", "source": ...}` where the variants are the same
  performance in different tones (DI, amps, renders).

Datasets (each optional, skipped when its folder is missing):
- EGDB (Chen et al., ICASSP 2022): 240 tabs played on electric guitar, DI
  plus amp renders, and 5 real recordings. Tabs with id % 5 == 0 are held
  out for testing.
- Guitar-TECHS (Pedroza et al., ICASSP 2025, CC BY 4.0): DI and miked amp,
  MIDI from a hexaphonic pickup. Singlenotes/chords/scales train, the
  musical excerpts (P3) and techniques are test only.
- IDMT-SMT-Guitar (Kehling et al., DAFx 2014, CC BY-NC-ND 4.0): test only,
  with per-note expression styles (bending, slide, vibrato...).
- GuitarSet (Xi et al., ISMIR 2018, CC BY 4.0), with `--guitarset`: players
  00-02 (mic recordings) train, to keep acoustic guitar in the mix; score
  the result on players 03-05 (`GVA_EVAL_PLAYERS=03,04,05`).
- GOAT (Loth et al., ISMIR 2025, non-commercial): DI plus 5 amp renders per
  item, fine-aligned MIDI and Guitar Pro tabs. `--goat <dir with
  metadata.csv>`; the official test split is test, the rest train (only
  for non-shipped research models). Techniques per note come from the tab.

Preset renders: link the training DI files into a folder (EGDB as
`<id>.wav`, Guitar-TECHS as `<part>__<file>.wav`, e.g.
`P1_chords__directinput_C.wav`) and run `render_tones --rate 16000 --out
<renders>/<egdb|techs> ...` on it.

Usage: python prepare_data.py --root ~/.cache/gva_test --out ~/.cache/gva_test/prepared
       [--renders ~/.cache/gva_test/renders] [--goat DIR]
"""

import argparse
import json
import re
import xml.etree.ElementTree as ET
from pathlib import Path

import mido

MIN_NOTE = 0.03  # seconds; hexaphonic pickups emit blips shorter than this


def midi_notes(path):
    """(onset, offset, pitch, velocity) of every note in a MIDI file."""
    m = mido.MidiFile(path)
    out, on, now, tempo = [], {}, 0.0, 500000
    for msg in mido.merge_tracks(m.tracks):
        now += mido.tick2second(msg.time, m.ticks_per_beat, tempo)
        if msg.type == "set_tempo":
            tempo = msg.tempo
        if msg.type not in ("note_on", "note_off"):
            continue
        key = (msg.channel, msg.note)
        if msg.type == "note_on" and msg.velocity > 0:
            if key in on:
                s, v = on.pop(key)
                out.append((s, now, msg.note, v))
            on[key] = (now, msg.velocity)
        elif key in on:
            s, v = on.pop(key)
            out.append((s, now, msg.note, v))
    for (ch, note), (s, v) in on.items():
        out.append((s, now, note, v))
    return sorted(n for n in out if n[1] - n[0] >= MIN_NOTE)


def write_notes(path, notes):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for on, off, p, v, *tech in notes:
            extra = f" {tech[0]}" if tech else ""
            f.write(f"{on:.4f} {off:.4f} {int(p)} {int(v)}{extra}\n")
    return path


def renders_of(renders, dataset, name):
    """Preset renders made by `render_tones` into
    `<renders>/<dataset>/<preset>/<name>`."""
    if renders is None:
        return []
    base = renders / dataset
    return sorted(str(p) for p in base.glob(f"*/{name}")) if base.exists() else []


def egdb(root, out, renders, train, evals):
    d = root / "egdb"
    if not (d / "audio_label").exists():
        return
    tones = sorted(p.name[len("audio_"):] for p in d.glob("audio_*") if p.name != "audio_label")
    for label in sorted((d / "audio_label").glob("*.midi"), key=lambda p: int(p.stem)):
        i = int(label.stem)
        notes = write_notes(out / "notes/egdb" / f"{i}.txt", midi_notes(label))
        audio = [d / f"audio_{t}" / f"{i}.wav" for t in tones]
        audio = [a for a in audio if a.exists()]
        if not audio:
            continue
        if i % 5 == 0:
            for a in audio:
                evals["egdb"].append((a, notes, f"egdb/{a.parent.name[6:]}", ""))
        else:
            di = d / "audio_DI" / f"{i}.wav"
            variants = [str(a) for a in audio] + renders_of(renders, "egdb", di.name)
            train.append({"audio": variants, "notes": str(notes), "source": "egdb"})
    for clip in sorted((d / "RealData/Audio").glob("*.wav")):
        label = d / "RealData/Label" / f"{clip.stem}.mid"
        if label.exists():
            notes = write_notes(out / "notes/egdb_real" / f"{clip.stem}.txt", midi_notes(label))
            evals["egdb"].append((clip, notes, "egdb/real", ""))


TECHNIQUE_LABELS = {
    "Bendings": "bend",
    "Vibrato": "vibrato",
    "PalmMute": "none",
}


def guitar_techs(root, out, renders, train, evals):
    d = root / "guitar_techs"
    if not d.exists():
        return
    for part in sorted(p for p in d.iterdir() if p.is_dir()):
        kind = part.name.split("_", 1)[1]
        for mid in sorted((part / "midi").glob("*.mid")):
            take = mid.stem[len("midi_"):]
            notes = write_notes(out / "notes/techs" / part.name / f"{take}.txt", midi_notes(mid))
            audio = {
                src: part / "audio" / src / f"{src}_{take}.wav"
                for src in ("directinput", "micamp")
            }
            audio = {k: v for k, v in audio.items() if v.exists()}
            if not audio:
                continue
            if kind == "music":
                for src, a in audio.items():
                    evals["techs"].append((a, notes, f"techs/music-{src}", ""))
            elif kind == "techniques":
                label = TECHNIQUE_LABELS.get(take)
                if label:
                    for src, a in audio.items():
                        evals["techs_techniques"].append(
                            (a, notes, f"techs/techniques-{src}", label)
                        )
            else:
                variants = [str(a) for a in audio.values()]
                if "directinput" in audio:
                    # render_tones inputs are linked as `<part>__<file>`.
                    name = f"{part.name}__{audio['directinput'].name}"
                    variants += renders_of(renders, "techs", name)
                train.append({"audio": variants, "notes": str(notes), "source": f"techs/{kind}"})


IDMT_STYLES = {
    "BE": "bend",
    "SL": "slide",
    "VI": "vibrato",
    "NO": "none",
    "HA": "harmonic",
    "DN": "dead",
}


def idmt(root, out, evals):
    d = root / "idmt"
    for x in sorted(d.glob("**/annotation/*.xml")):
        wav = x.parent.parent / "audio" / f"{x.stem}.wav"
        if not wav.exists():
            continue
        try:
            tree = ET.parse(x)
        except ET.ParseError:
            continue
        notes = []
        for ev in tree.iter("event"):
            try:
                on = float(ev.findtext("onsetSec"))
                off = float(ev.findtext("offsetSec"))
                p = int(ev.findtext("pitch"))
            except (TypeError, ValueError):
                continue
            style = IDMT_STYLES.get(ev.findtext("expressionStyle") or "NO", "none")
            notes.append((on, off, p, 0, style))
        if not notes:
            continue
        subset = next((s for s in wav.parts if re.match(r"dataset\d", s)), "idmt")
        n = write_notes(out / "notes/idmt" / subset / f"{x.stem}.txt", sorted(notes))
        evals["idmt"].append((wav, n, f"idmt/{subset}", ""))


def guitarset(root, out, train):
    import json as _json

    d = root / "guitarset"
    for jams in sorted((d / "annotation").glob("0[0-2]_*.jams")):
        wav = d / "audio" / f"{jams.stem}_mic.wav"
        if not wav.exists():
            continue
        notes = []
        for a in _json.load(open(jams))["annotations"]:
            if a["namespace"] != "note_midi":
                continue
            data = a["data"]
            if isinstance(data, dict):
                data = [dict(time=t, duration=u, value=v) for t, u, v in zip(data["time"], data["duration"], data["value"])]
            notes += [(n["time"], n["time"] + n["duration"], round(n["value"]), 0) for n in data]
        n = write_notes(out / "notes/guitarset" / f"{jams.stem}.txt", sorted(notes))
        train.append({"audio": [str(wav)], "notes": str(n), "source": "guitarset"})


GOAT_PRIORITY = ("bend", "slide", "vibrato", "legato", "harmonic", "palm", "dead")


def goat_tab_notes(gp5):
    """(score seconds, pitch, technique) of every struck note of the tab's
    first track, in time order (needs PyGuitarPro)."""
    import guitarpro

    song = guitarpro.parse(str(gp5))
    track = song.tracks[0]
    tuning = {s.number: s.value for s in track.strings}
    tempo, now, out = float(song.tempo), 0.0, []
    for measure in track.measures:
        start = now
        length = 0.0
        for voice in measure.voices[:1]:
            t = start
            for beat in voice.beats:
                mix = beat.effect.mixTableChange
                if mix is not None and mix.tempo is not None and mix.tempo.value > 0:
                    tempo = float(mix.tempo.value)
                for n in beat.notes:
                    if n.type == guitarpro.NoteType.tie:
                        continue
                    e = n.effect
                    labels = set()
                    if e.isBend:
                        labels.add("bend")
                    if e.slides:
                        labels.add("slide")
                    if e.vibrato or beat.effect.vibrato:
                        labels.add("vibrato")
                    if e.hammer:
                        labels.add("legato")
                    if e.isHarmonic:
                        labels.add("harmonic")
                    if e.palmMute:
                        labels.add("palm")
                    if n.type == guitarpro.NoteType.dead:
                        labels.add("dead")
                    label = next((x for x in GOAT_PRIORITY if x in labels), "none")
                    out.append((t, tuning[n.string] + n.value, label))
                t += beat.duration.time / 960 * 60 / tempo
            length = max(length, t - start)
        now = start + length
    return sorted(out)


def goat(goat_dir, out, renders, train, evals):
    """GOAT with its official split; techniques per note from the Guitar Pro
    tab, carried to the fine-aligned MIDI notes by pitch and order."""
    import csv

    if goat_dir is None or not (goat_dir / "metadata.csv").exists():
        return
    for row in csv.DictReader(open(goat_dir / "metadata.csv")):
        item = row["item"]
        d = goat_dir / "data" / item
        aligned = d / f"{item}_fine_aligned.mid"
        if not aligned.exists():
            continue
        notes = midi_notes(aligned)
        # Tab notes -> unaligned MIDI (same count and order) -> aligned notes.
        labels = []
        try:
            tab = goat_tab_notes(d / f"{item}.gp5")
            unaligned = midi_notes(d / f"{item}.mid")
            key = lambda n: (round(n[0], 3), n[2])
            unaligned = sorted(unaligned, key=key)
            tab = sorted(tab, key=lambda n: (round(n[0], 3), n[1]))
            if len(tab) == len(unaligned):
                pairs = [(u[0], u[2], t[2]) for u, t in zip(unaligned, tab) if u[2] == t[1]]
                for on, off, p, v in notes:
                    near = [x for x in pairs if x[1] == p and abs(x[0] - on) < 0.3]
                    labels.append(min(near, key=lambda x: abs(x[0] - on))[2] if near else "none")
        except Exception as err:  # unreadable tab: no technique labels
            print(f"{item}: no techniques ({err})")
        if labels:
            notes = [n + (lab,) for n, lab in zip(notes, labels)]
        path = write_notes(out / "notes/goat" / f"{item}.txt", notes)
        audio = {"di": d / f"{item}.wav"}
        audio.update({f"amp_{k}": d / f"{item}_amp_{k}.wav" for k in range(1, 6)})
        audio = {k: v for k, v in audio.items() if v.exists()}
        if row["split"] == "test":
            for tone, a in audio.items():
                evals["goat"].append((a, path, f"goat/{tone}", ""))
        else:
            variants = [str(a) for a in audio.values()] + renders_of(renders, "goat", f"{item}.wav")
            train.append({"audio": variants, "notes": str(path), "source": "goat"})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--renders", type=Path)
    ap.add_argument("--goat", type=Path)
    ap.add_argument("--guitarset", action="store_true")
    args = ap.parse_args()
    root, out = args.root.expanduser(), args.out.expanduser()
    out.mkdir(parents=True, exist_ok=True)
    renders = args.renders.expanduser() if args.renders else None
    train = []
    evals = {k: [] for k in ("egdb", "techs", "techs_techniques", "idmt", "goat")}
    egdb(root, out, renders, train, evals)
    guitar_techs(root, out, renders, train, evals)
    idmt(root, out, evals)
    goat(args.goat.expanduser() if args.goat else None, out, renders, train, evals)
    if args.guitarset:
        guitarset(root, out, train)
    with open(out / "train.jsonl", "w") as f:
        for item in train:
            f.write(json.dumps(item) + "\n")
    for name, rows in evals.items():
        if rows:
            with open(out / f"eval_{name}.tsv", "w") as f:
                for a, n, g, t in rows:
                    f.write(f"{a}\t{n}\t{g}\t{t}\n")
    print(f"train: {len(train)} recordings, {sum(len(t['audio']) for t in train)} variants")
    for name, rows in evals.items():
        print(f"eval_{name}: {len(rows)} recordings")


if __name__ == "__main__":
    main()
