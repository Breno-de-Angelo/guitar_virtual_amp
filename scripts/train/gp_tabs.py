"""Guitar Pro tabs for the tab-to-audio aligner (`align_tabs.py`): reads
GP3-5 (PyGuitarPro) and GP6/7/8 (`.gpx` BCFZ container, `.gp` zip, both
GPIF XML), expands repeats like the app, lists every track's notes in score
time, and writes copies with the repeats written out.

Conventions follow the app's importer (guitar_core/src/shared/tab/import.rs)
and timeline (timeline.rs), so bar indices and the playback order match what
the Tabs page uses for `.gvtab.json` sync:
- strings are numbered from 1 = highest, tunings listed from string 1;
- pitch = open string + capo + fret;
- repeats: GP3-5 `repeatClose` is the number of jumps back, GPIF `count`
  the number of plays (jumps = count - 1); alternate endings are pass
  bitmasks; direction signs (D.S., coda) are ignored, as in the app;
- tempo: GP3-5 song tempo plus mix-table changes; GPIF tempo automations,
  one per bar at its start, value's first number (quarter notes), as the
  `guitarpro` crate reads them.
Score time is seconds from the first bar at the notated tempo ("score time"
in timeline.rs).
"""
import copy
import io
import struct
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

TPQ = 960  # ticks per quarter note, as Guitar Pro and the app
MAX_OCCURRENCES = 20000
TECH_PRIORITY = ("bend", "slide", "vibrato", "legato", "harmonic", "palm")
# Sounding interval above the open string of a natural harmonic touched at a fret.
NATURAL_HARMONIC = {12: 12, 7: 19, 19: 19, 5: 24, 24: 24, 4: 28, 9: 28, 16: 28}
GPIF_DYNAMICS = {"PPP": 15, "PP": 31, "P": 47, "MP": 63, "MF": 79, "F": 95, "FF": 111, "FFF": 127}


@dataclass
class Bar:
    numerator: int = 4
    denominator: int = 4
    tempo_changes: list = field(default_factory=list)  # (tick, bpm)
    repeat_open: bool = False
    repeat_jumps: int = 0
    alternatives: int = 0  # bit n: played on pass n + 1; 0 = every pass

    @property
    def length_ticks(self):
        return max(1, self.numerator) * TPQ * 4 // max(1, self.denominator)


@dataclass
class TabNote:
    string: int  # 1 = highest
    fret: int
    velocity: int = 95
    tie: bool = False
    dead: bool = False
    grace: int = 0  # ticks before the beat (0 = not a grace note)
    let_ring: bool = False
    techniques: set = field(default_factory=set)
    harmonic_natural: bool = False


@dataclass
class Beat:
    start: int  # ticks from the bar start
    duration: int
    notes: list


@dataclass
class Track:
    name: str
    tuning: list  # open-string MIDI pitches from string 1 (highest)
    capo: int = 0
    kind: str = "guitar"  # guitar, bass, drums, other
    program: int = -1
    bars: list = field(default_factory=list)  # per bar: list of voices, each a list of Beat


@dataclass
class Tab:
    path: Path
    format: str  # gp3, gp4, gp5, gpx, gp
    title: str = ""
    artist: str = ""
    initial_tempo: float = 120.0
    bars: list = field(default_factory=list)
    tracks: list = field(default_factory=list)
    sync_points: list = field(default_factory=list)  # (bar, occurrence, audio secs)
    backing_audio: bool = False

    def order(self):
        return playback_order(self.bars)

    def occurrences(self):
        """[(bar index, start secs, end secs, segments)] in playback order;
        segments are (tick, secs, bpm) as in timeline.rs."""
        out, bpm, secs = [], max(1.0, self.initial_tempo), 0.0
        for b in self.order():
            bar = self.bars[b]
            length = bar.length_ticks
            segs, seg_tick, seg_secs = [], 0, secs
            for tick, new in sorted(bar.tempo_changes):
                tick = min(max(tick, 0), length)
                if tick > seg_tick:
                    segs.append((seg_tick, seg_secs, bpm))
                    seg_secs += (tick - seg_tick) / TPQ * 60 / bpm
                    seg_tick = tick
                bpm = max(1.0, new)
            segs.append((seg_tick, seg_secs, bpm))
            end = seg_secs + (length - seg_tick) / TPQ * 60 / bpm
            out.append((b, secs, end, segs))
            secs = end
        return out

    def notes(self, track_index):
        """Struck notes of a track in playback order, ties merged: dicts with
        score `start`/`end` (secs), `pitch`, `string`, `fret`, `velocity`,
        `technique` (none or the first of TECH_PRIORITY), `bar`,
        `occurrence`. Dead notes are left out (no pitch)."""
        track = self.tracks[track_index]
        occ = self.occurrences()
        out = []
        ringing = {}  # string -> index in out of the note sounding on it
        for k, (b, start, end, segs) in enumerate(occ):
            if b >= len(track.bars):
                continue

            def secs(tick, segs=segs):
                i = max(0, sum(1 for s in segs if s[0] <= tick) - 1)
                t0, s0, bpm = segs[i]
                return s0 + (tick - t0) / TPQ * 60 / bpm

            for voice in track.bars[b]:
                for beat in voice:
                    on, off = secs(beat.start), secs(beat.start + beat.duration)
                    for n in beat.notes:
                        if n.string < 1 or n.string > len(track.tuning):
                            continue
                        if n.tie:
                            j = ringing.get(n.string)
                            if j is not None:
                                out[j]["end"] = max(out[j]["end"], off)
                            continue
                        if n.dead:
                            ringing.pop(n.string, None)
                            continue
                        open_pitch = track.tuning[n.string - 1] + track.capo
                        pitch = open_pitch + n.fret
                        if n.harmonic_natural and n.fret in NATURAL_HARMONIC:
                            pitch = open_pitch + NATURAL_HARMONIC[n.fret]
                        if not 0 < pitch < 128:
                            continue
                        t_on, t_off = on, off
                        if n.grace:
                            t_on = secs(beat.start - n.grace) if beat.start >= n.grace else on - n.grace / TPQ * 60 / segs[0][2]
                            t_off = on
                        j = ringing.get(n.string)
                        if j is not None and out[j]["start"] < t_on and out[j]["end"] > t_on:
                            out[j]["end"] = t_on  # a new pick ends the note ringing on this string
                        tech = next((t for t in TECH_PRIORITY if t in n.techniques), "none")
                        out.append(dict(start=t_on, end=max(t_off, t_on + 0.01), pitch=pitch, string=n.string,
                                        fret=n.fret, velocity=n.velocity, technique=tech, bar=b, occurrence=k,
                                        let_ring=n.let_ring))
                        ringing[n.string] = len(out) - 1
        for i, n in enumerate(out):
            if n.pop("let_ring"):
                nxt = [m["start"] for m in out[i + 1:] if m["string"] == n["string"] and m["start"] > n["start"]]
                n["end"] = max(n["end"], min(nxt[0] if nxt else n["start"] + 4.0, n["start"] + 4.0))
        return sorted(out, key=lambda n: (n["start"], n["pitch"]))


def playback_order(bars):
    """Port of timeline.rs `playback_order`: repeats and alternate endings
    expanded into the order bars are played in."""
    order, i, repeat_start, passno = [], 0, 0, 1
    jumps_taken = [0] * len(bars)
    jumped_back = False
    while i < len(bars) and len(order) < MAX_OCCURRENCES:
        bar = bars[i]
        if bar.repeat_open and not jumped_back:
            repeat_start, passno = i, 1
            for j in range(i, len(bars)):
                jumps_taken[j] = 0
        jumped_back = False
        if bar.alternatives and passno <= 8 and not bar.alternatives & (1 << (passno - 1)):
            i += 1
            continue
        order.append(i)
        if bar.repeat_jumps > 0 and jumps_taken[i] < bar.repeat_jumps:
            jumps_taken[i] += 1
            passno += 1
            i, jumped_back = repeat_start, True
            continue
        if bar.repeat_jumps > 0:
            passno, repeat_start = 1, i + 1
        i += 1
    return order


# ---------------------------------------------------------------- format detection

def detect_format(data, suffix):
    if data[:2] == b"PK":
        return "gp"
    if data[:4] in (b"BCFZ", b"BCFS"):
        return "gpx"
    head = data[1:31].decode("latin-1", "replace")
    for v in ("5", "4", "3"):
        if f"v{v}." in head:
            return f"gp{v}"
    s = suffix.lower().lstrip(".")
    return s if s in ("gp3", "gp4", "gp5", "gpx", "gp") else None


def load(path):
    path = Path(path)
    data = path.read_bytes()
    fmt = detect_format(data, path.suffix)
    if fmt is None:
        raise ValueError(f"{path}: not a Guitar Pro file")
    if fmt in ("gp3", "gp4", "gp5"):
        return _load_legacy(path, fmt)
    xml, assets = gpif_xml(data, fmt)
    return _load_gpif(path, fmt, xml, assets)


# ---------------------------------------------------------------- GP3-5

def _kind(name, tuning, program, percussion, instrument_type=""):
    name_l, itype = name.lower(), instrument_type.lower()
    if percussion or (tuning and all(p == 0 for p in tuning)) or "drum" in itype or any(
        w in name_l for w in ("drum", "percussion", "bateria")
    ):
        return "drums"
    if "bass" in itype or 32 <= program <= 39 or "bass" in name_l or (len(tuning) <= 6 and tuning and tuning[0] <= 43):
        return "bass"
    if "guitar" in itype or 24 <= program <= 31:
        return "guitar"
    if itype or 0 <= program < 24 or program >= 40:
        return "other"
    return "guitar" if len(tuning) >= 6 else "other"


def _load_legacy(path, fmt):
    import guitarpro

    song = guitarpro.parse(str(path))
    tab = Tab(path=path, format=fmt, title=song.title.strip(), artist=song.artist.strip(),
              initial_tempo=float(song.tempo) if song.tempo > 0 else 120.0)
    for h in song.measureHeaders:
        tab.bars.append(Bar(
            numerator=max(1, h.timeSignature.numerator),
            denominator=max(1, h.timeSignature.denominator.value),
            repeat_open=bool(h.isRepeatOpen),
            repeat_jumps=max(0, h.repeatClose) if h.repeatClose > 0 else 0,
            alternatives=int(h.repeatAlternative or 0) & 0xFF,
        ))
    for t in song.tracks:
        strings = sorted(t.strings, key=lambda s: s.number)
        tuning = [s.value for s in strings]
        percussion = bool(t.isPercussionTrack or getattr(t.channel, "isPercussionChannel", False))
        program = int(getattr(t.channel, "instrument", -1))
        track = Track(name=t.name.strip(), tuning=tuning, capo=max(0, t.offset),
                      kind=_kind(t.name, tuning, program, percussion), program=program)
        for b, m in enumerate(t.measures):
            voices = []
            for v in m.voices:
                beats, cursor = [], 0
                for beat in v.beats:
                    dur = _legacy_ticks(beat.duration)
                    start, cursor = cursor, cursor + dur
                    mix = beat.effect.mixTableChange
                    if mix is not None and mix.tempo is not None and mix.tempo.value > 0 and b < len(tab.bars):
                        tab.bars[b].tempo_changes.append((start, float(mix.tempo.value)))
                    if beat.status != guitarpro.BeatStatus.normal:
                        continue
                    notes = []
                    for n in beat.notes:
                        if n.type == guitarpro.NoteType.rest or n.string < 1:
                            continue
                        e = n.effect
                        techs = set()
                        if e.isBend:
                            techs.add("bend")
                        if e.slides:
                            techs.add("slide")
                        if e.vibrato or beat.effect.vibrato:
                            techs.add("vibrato")
                        if e.hammer:
                            techs.add("legato")
                        if e.isHarmonic:
                            techs.add("harmonic")
                        if e.palmMute:
                            techs.add("palm")
                        natural = e.isHarmonic and getattr(e.harmonic, "type", 0) == 1
                        if e.isGrace and n.type != guitarpro.NoteType.tie:
                            g = e.grace  # played just before the note, on its string
                            notes.append(TabNote(string=n.string, fret=g.fret, velocity=g.velocity,
                                                 dead=bool(g.isDead), grace=g.durationTime,
                                                 techniques={"legato"} if g.transition.name == "hammer" else set()))
                        notes.append(TabNote(string=n.string, fret=n.value, velocity=n.velocity,
                                             tie=n.type == guitarpro.NoteType.tie,
                                             dead=n.type == guitarpro.NoteType.dead, let_ring=bool(e.letRing),
                                             techniques=techs, harmonic_natural=natural))
                    if notes:
                        beats.append(Beat(start, dur, notes))
                voices.append(beats)
            track.bars.append(voices)
        track.bars += [[] for _ in range(len(tab.bars) - len(track.bars))]
        tab.tracks.append(track)
    for bar in tab.bars:
        bar.tempo_changes = sorted({t: v for t, v in bar.tempo_changes}.items())
    return tab


def _legacy_ticks(d):
    ticks = TPQ * 4 // max(1, d.value)
    if getattr(d, "isDoubleDotted", False):
        ticks = ticks * 7 // 4
    elif d.isDotted:
        ticks = ticks * 3 // 2
    if d.tuplet.enters > 1 and d.tuplet.times > 0:
        ticks = ticks * d.tuplet.times // d.tuplet.enters
    return max(1, ticks)


# ---------------------------------------------------------------- GPIF (GP6/7/8)

def _bcfz_decompress(data):
    """GP6 BCFZ: LZ77-style bit stream (as guitarpro-0.5.0 src/io/gpx.rs)."""
    expected = struct.unpack("<i", data[4:8])[0]
    src, pos, out = data[8:], 0, bytearray()

    def bit():
        nonlocal pos
        i = pos >> 3
        if i >= len(src):
            return 0
        pos += 1
        return (src[i] >> (7 - ((pos - 1) & 7))) & 1

    def bits(n):
        v = 0
        for _ in range(n):
            v = (v << 1) | bit()
        return v

    def bits_rev(n):
        v = 0
        for i in range(n):
            v |= bit() << i
        return v

    while len(out) < expected and (pos >> 3) < len(src):
        if bit():
            size_bits = bits(4)
            offset, size = bits_rev(size_bits), bits_rev(size_bits)
            if offset == 0 or offset > len(out):
                raise ValueError("BCFZ: bad back-reference")
            start = len(out) - offset
            for i in range(size):
                out.append(out[start + i % offset])
        else:
            for _ in range(bits_rev(2)):
                out.append(bits(8))
    return bytes(out[:expected])


def _bcfs_files(data):
    disk, files, sector = data[4:], {}, 0x1000
    off = sector
    while off + 3 < len(disk):
        if struct.unpack("<i", disk[off:off + 4])[0] == 2 and off + 0x98 <= len(disk):
            name = disk[off + 4:off + 4 + 127].split(b"\0")[0].decode("utf-8", "replace")
            size = struct.unpack("<i", disk[off + 0x8C:off + 0x90])[0]
            chunks, idx = [], off + 0x94
            while idx + 4 <= off + sector:
                block = struct.unpack("<i", disk[idx:idx + 4])[0]
                if block == 0:
                    break
                chunks.append(disk[block * sector:(block + 1) * sector])
                idx += 4
            files[name] = b"".join(chunks)[:size]
        off += sector
    return files


def gpif_xml(data, fmt):
    """(score.gpif text, names of embedded audio assets)."""
    if fmt == "gp":
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            assets = [n for n in z.namelist() if n.startswith("Content/Assets/")]
            return z.read("Content/score.gpif").decode("utf-8"), assets
    if data[:4] == b"BCFZ":
        data = _bcfz_decompress(data)
    files = _bcfs_files(data)
    name = next(n for n in files if n.endswith("score.gpif"))
    return files[name].decode("utf-8"), []


def _ids(text):
    return [int(v) for v in (text or "").split()]


def _props(el):
    """name -> Property element of an element's <Properties>."""
    return {p.get("name"): p for p in el.findall("./Properties/Property")} if el is not None else {}


def _gpif_tuning(track):
    for props in [track] + track.findall("./Staves/Staff"):
        p = _props(props).get("Tuning")
        if p is not None and p.findtext("Pitches"):
            return [int(v) for v in p.findtext("Pitches").split()]
    return [40, 45, 50, 55, 59, 64]


def _gpif_capo(track):
    for props in [track] + track.findall("./Staves/Staff"):
        p = _props(props).get("CapoFret")
        if p is not None and (p.findtext("Fret") or "").strip():
            return int(p.findtext("Fret"))
    return 0


def _load_gpif(path, fmt, xml, assets):
    root = ET.fromstring(xml)
    tab = Tab(path=path, format=fmt, title=(root.findtext("./Score/Title") or "").strip(),
              artist=(root.findtext("./Score/Artist") or "").strip(),
              backing_audio=any(a.endswith(".mp3") for a in assets))
    tempos = {}
    for a in root.findall("./MasterTrack/Automations/Automation"):
        kind = a.findtext("Type")
        if kind == "Tempo":
            try:
                tempos[int(a.findtext("Bar"))] = float(int(float((a.findtext("Value") or "120").split()[0])))
            except ValueError:
                pass
        elif kind == "SyncPoint":
            v = a.find("Value")
            if v is not None and v.findtext("BarIndex") and v.findtext("FrameOffset"):
                tab.sync_points.append((int(v.findtext("BarIndex")), int(v.findtext("BarOccurrence") or 0),
                                        float(v.findtext("FrameOffset")) / 44100.0))
    tab.initial_tempo = tempos.get(0, 120.0)
    master_bars = root.findall("./MasterBars/MasterBar")
    bar_ids = []
    for i, mb in enumerate(master_bars):
        num, den = (mb.findtext("Time") or "4/4").split("/")
        bar = Bar(numerator=int(num), denominator=int(den))
        if i in tempos:
            bar.tempo_changes = [(0, tempos[i])]
        rep = mb.find("Repeat")
        if rep is not None:
            bar.repeat_open = rep.get("start") == "true"
            if rep.get("end") == "true":
                bar.repeat_jumps = max(1, int(rep.get("count") or 1) - 1)
        for tok in (mb.findtext("AlternateEndings") or "").split():
            if tok.isdigit() and 1 <= int(tok) <= 8:
                bar.alternatives |= 1 << (int(tok) - 1)
        tab.bars.append(bar)
        bar_ids.append(_ids(mb.findtext("Bars")))
    bars = {int(b.get("id")): b for b in root.findall("./Bars/Bar")}
    voices = {int(v.get("id")): v for v in root.findall("./Voices/Voice")}
    beats = {int(b.get("id")): b for b in root.findall("./Beats/Beat")}
    notes = {int(n.get("id")): n for n in root.findall("./Notes/Note")}
    rhythms = {int(r.get("id")): r for r in root.findall("./Rhythms/Rhythm")}
    for t_idx, tr in enumerate(root.findall("./Tracks/Track")):
        tuning = list(reversed(_gpif_tuning(tr)))  # GPIF lists the lowest string first
        program = int(tr.findtext("./GeneralMidi/Program") or tr.findtext("./Sounds/Sound/MIDI/Program") or -1)
        channel = tr.findtext("./GeneralMidi/PrimaryChannel") or tr.findtext("./MidiConnection/PrimaryChannel")
        name = (tr.findtext("Name") or "").strip()
        itype = tr.findtext("./InstrumentSet/Type") or ""
        track = Track(name=name, tuning=tuning, capo=_gpif_capo(tr), program=program,
                      kind=_kind(name, tuning, program, channel == "9", itype))
        n_str = len(tuning)
        velocity = 95
        for i in range(len(master_bars)):
            ids = bar_ids[i]
            bar = bars.get(ids[t_idx]) if t_idx < len(ids) else None
            vs = []
            for vid in _ids(bar.findtext("Voices")) if bar is not None else []:
                if vid < 0 or vid not in voices:
                    continue
                out, cursor = [], 0
                for bid in _ids(voices[vid].findtext("Beats")):
                    be = beats.get(bid)
                    if be is None:
                        continue
                    r = rhythms.get(int(be.find("Rhythm").get("ref"))) if be.find("Rhythm") is not None else None
                    dur = _gpif_ticks(r)
                    velocity = GPIF_DYNAMICS.get(be.findtext("Dynamic") or "", velocity)
                    grace = be.findtext("GraceNotes")
                    start = cursor
                    if grace:
                        g_ticks = dur if grace == "BeforeBeat" else 0
                    else:
                        cursor += dur
                    beat_vibrato = be.findtext("Vibrato") is not None
                    ns = []
                    for nid in _ids(be.findtext("Notes")):
                        ne = notes.get(nid)
                        if ne is None:
                            continue
                        p = _props(ne)
                        if "String" not in p or "Fret" not in p:
                            continue
                        string = n_str - int(p["String"].findtext("String"))
                        techs = set()
                        bend = p.get("Bended")
                        if bend is not None and bend.find("Enable") is not None:
                            techs.add("bend")
                        if "Slide" in p:
                            techs.add("slide")
                        if ne.find("Vibrato") is not None or beat_vibrato:
                            techs.add("vibrato")
                        if "HopoOrigin" in p:
                            techs.add("legato")
                        htype = p["HarmonicType"].findtext("HType") if "HarmonicType" in p else None
                        if htype:
                            techs.add("harmonic")
                        if "PalmMuted" in p:
                            techs.add("palm")
                        tie = ne.find("Tie")
                        ns.append(TabNote(string=string, fret=int(p["Fret"].findtext("Fret")), velocity=velocity,
                                          tie=tie is not None and tie.get("destination") == "true",
                                          dead="Muted" in p or "Dead" in p, let_ring=ne.find("LetRing") is not None,
                                          grace=(g_ticks or 1) if grace else 0, techniques=techs,
                                          harmonic_natural=htype == "Natural"))
                    if ns:
                        out.append(Beat(start, dur, ns))
                vs.append(out)
            track.bars.append(vs)
        tab.tracks.append(track)
    return tab


def _gpif_ticks(r):
    if r is None:
        return TPQ
    value = {"Whole": 1, "Half": 2, "Quarter": 4, "Eighth": 8, "16th": 16, "32nd": 32, "64th": 64,
             "128th": 128}.get(r.findtext("NoteValue"), 4)
    ticks = TPQ * 4 // value
    dot = r.find("AugmentationDot")
    if dot is not None:
        ticks = ticks * 7 // 4 if dot.get("count") == "2" else ticks * 3 // 2
    tup = r.find("PrimaryTuplet")
    if tup is not None and int(tup.get("num", 1)) > 1:
        ticks = ticks * int(tup.get("den")) // int(tup.get("num"))
    return max(1, ticks)


# ---------------------------------------------------------------- unrolled copies

def write_unrolled(tab, dest):
    """Writes a copy of the tab with every bar occurrence as its own bar (no
    repeats or alternate endings) and without embedded audio, so the app's
    per-bar sidecar sync (`TabMetadata::sync_map`, first occurrence of each
    bar) covers every pass. GP3-5 stay in their format; GPX becomes `.gp`.
    Returns the path written."""
    dest = Path(dest)
    order = tab.order()
    if tab.format in ("gp3", "gp4", "gp5"):
        import guitarpro

        song = guitarpro.parse(str(tab.path))
        headers = song.measureHeaders
        new_headers = []
        start = headers[0].start if headers else TPQ
        for k, b in enumerate(order):
            h = copy.deepcopy(headers[b])
            h.number, h.start = k + 1, start
            h.isRepeatOpen, h.repeatClose, h.repeatAlternative = False, -1, 0
            h.direction = h.fromDirection = None
            start += h.length
            new_headers.append(h)
        for t in song.tracks:
            measures = []
            for k, b in enumerate(order):
                old = t.measures[b]
                memo = {id(song): song, id(t): t, id(old.header): new_headers[k]}
                measures.append(copy.deepcopy(old, memo))
            t.measures = measures
        song.measureHeaders = new_headers
        dest = dest.with_suffix("." + tab.format)
        guitarpro.write(song, str(dest))
        return dest
    data = tab.path.read_bytes()
    xml, _ = gpif_xml(data, tab.format)
    root = ET.fromstring(xml)
    mbs = root.find("MasterBars")
    old = mbs.findall("MasterBar")
    for mb in old:
        mbs.remove(mb)
    for b in order:
        mb = copy.deepcopy(old[b])
        for tag in ("Repeat", "AlternateEndings", "Directions"):
            for el in mb.findall(tag):
                mb.remove(el)
        mbs.append(mb)
    occurrences = {}
    for k, b in enumerate(order):
        occurrences.setdefault(b, []).append(k)
    for autos in root.iter("Automations"):
        items = list(autos)
        for a in items:
            autos.remove(a)
        for a in items:
            if a.findtext("Type") == "SyncPoint":
                continue
            bar = a.findtext("Bar")
            if bar is None or not bar.strip().lstrip("-").isdigit():
                autos.append(a)
                continue
            for k in occurrences.get(int(bar), []):
                c = copy.deepcopy(a)
                c.find("Bar").text = str(k)
                autos.append(c)
    for parent in list(root.iter()):
        for tag in ("BackingTrack", "Assets", "AudioTracks"):
            for el in parent.findall(tag):
                parent.remove(el)
    text = '<?xml version="1.0" encoding="utf-8"?>\n' + ET.tostring(root, encoding="unicode")
    dest = dest.with_suffix(".gp")
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
        if tab.format == "gp":
            with zipfile.ZipFile(io.BytesIO(data)) as src:
                for item in src.infolist():
                    if item.filename == "Content/score.gpif" or item.filename.startswith("Content/Assets/"):
                        continue
                    z.writestr(item, src.read(item.filename))
        z.writestr("Content/score.gpif", text)
    return dest
