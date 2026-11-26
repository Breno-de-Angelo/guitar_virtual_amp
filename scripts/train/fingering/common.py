"""Shared pieces of the learned fingering model: note features, chord
grouping, decoding of per-note string probabilities into a playable
assignment, and playability counts.

A note is `(onset, offset, pitch)`; a labelled note adds the string
(1 = high e .. 6 = low E). Tunings list the open-string pitches from string
1, as `bench_tab.STANDARD` and the app's `InstrumentSpec::tuning`.

The model sees, per note, the fret it would have on each of the six strings
(or "can't be played there") plus timing and interval features, so it works
in fret space and does not depend on the tuning.
"""
import itertools
import os
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
# bench_tab.py / bench_notes.py live one folder up in the repo; on the Spark
# the copy is in ~/gva/scripts/train (GVA_TRAIN_SCRIPTS).
for p in (os.environ.get("GVA_TRAIN_SCRIPTS"), str(HERE.parent)):
    if p and (Path(p) / "bench_tab.py").exists():
        sys.path.insert(0, p)
        break

STANDARD = [64, 59, 55, 50, 45, 40]
MAX_FRET = 24
INVALID = MAX_FRET + 1  # fret index for "pitch not playable on this string"
CHORD_SECS = 0.05  # onsets within this of a chord's first note form a chord
N_CONT = 16


def sort_notes(notes):
    """Index order by onset, then pitch (low to high)."""
    return sorted(range(len(notes)), key=lambda i: (notes[i][0], notes[i][2]))


def chord_ids(onsets, window=CHORD_SECS):
    """Chord index per note (onsets sorted): a chord starts at a note more
    than `window` after the previous chord's first note."""
    out, start, cid = np.zeros(len(onsets), np.int64), None, -1
    for i, t in enumerate(onsets):
        if start is None or t - start > window:
            start, cid = t, cid + 1
        out[i] = cid
    return out


def candidate_frets(pitches, tuning):
    """[N, 6] fret of each pitch on each string, INVALID when out of range."""
    f = np.asarray(pitches, np.int64)[:, None] - np.asarray(tuning, np.int64)[None, :]
    return np.where((f >= 0) & (f <= MAX_FRET), f, INVALID)


def features(notes, tuning):
    """Model inputs for notes already in `sort_notes` order:
    frets [N, 6] int64, cont [N, N_CONT] float32, chord ids [N]."""
    n = len(notes)
    on = np.array([x[0] for x in notes], np.float64)
    off = np.array([x[1] for x in notes], np.float64)
    p = np.array([x[2] for x in notes], np.float64)
    frets = candidate_frets(p.astype(np.int64), tuning)
    cid = chord_ids(on)
    cont = np.zeros((n, N_CONT), np.float32)
    if n == 0:
        return frets, cont, cid
    dt_prev = np.diff(on, prepend=on[0] - 2.0)
    dt_next = np.diff(on, append=on[-1] + 2.0)
    cont[:, 0] = np.log1p(np.clip(dt_prev, 0, 4) / 0.05)
    cont[:, 1] = np.log1p(np.clip(dt_next, 0, 4) / 0.05)
    cont[:, 2] = np.clip(np.log(np.maximum(off - on, 0.01) / 0.25), -3, 3)
    size = np.bincount(cid)[cid]
    cont[:, 3] = size / 6.0
    first = np.searchsorted(cid, cid)  # index of each chord's first note
    rank = np.arange(n) - first  # notes are sorted by pitch inside a chord
    cont[:, 4] = rank / 5.0
    cont[:, 5] = size > 1
    pp = np.concatenate([[p[0]], p[:-1]])
    pn = np.concatenate([p[1:], [p[-1]]])
    cont[:, 6] = np.clip((p - pp) / 12, -2, 2)
    cont[:, 7] = np.clip((pn - p) / 12, -2, 2)
    cont[:, 8] = (p - 64) / 12
    cont[:, 9:15] = (np.asarray(tuning, np.float64) - STANDARD) / 2
    cont[:, 15] = np.clip((on - on[first]) / 0.05, 0, 2)
    return frets, cont, cid


def labels(notes, frets):
    """Target string index 0..5 per note, -100 when missing or unplayable."""
    y = np.full(len(notes), -100, np.int64)
    for i, x in enumerate(notes):
        s = x[3] if len(x) > 3 else 0
        if 1 <= s <= 6 and frets[i, s - 1] != INVALID:
            y[i] = s - 1
    return y


# ---------------------------------------------------------------- inference

def windowed(run, frets, cont, window=128, hop=64, extra=()):
    """Logits [N, 6] of a whole recording from `run(frets[1,T,6],
    cont[1,T,F], *extra[1,T,...]) -> logits[1,T,6]` over overlapping
    windows; each note takes the window where it is farthest from an edge.
    `extra`: more per-note inputs (the audio patches), windowed the same."""
    n = len(frets)
    if n <= window:
        return run(frets[None], cont[None], *(a[None] for a in extra))[0]
    out = np.zeros((n, 6), np.float32)
    best = np.full(n, -1.0)
    starts = list(range(0, n - window + 1, hop))
    if starts[-1] != n - window:
        starts.append(n - window)
    for s in starts:
        lg = run(frets[None, s : s + window], cont[None, s : s + window], *(a[None, s : s + window] for a in extra))[0]
        idx = np.arange(s, s + window)
        margin = np.minimum(idx - s, s + window - 1 - idx).astype(float)
        margin[idx == 0] = window
        margin[idx == n - 1] = window
        take = margin > best[idx]
        out[idx[take]] = lg[take]
        best[idx[take]] = margin[take]
    return out


def log_softmax_valid(logits, frets):
    lg = np.where(frets != INVALID, logits, -1e9).astype(np.float64)
    m = lg.max(1, keepdims=True)
    return lg - m - np.log(np.exp(lg - m).sum(1, keepdims=True))


def chord_options(lp, frets, idx, span_cost, top, pos_cost=0.0):
    """Best `top` assignments of distinct strings to the notes `idx` of one
    chord: (score, strings tuple, hand position or None). `pos_cost` per
    fret of hand position (lowest fretted note) favors low positions."""
    choices = [[s for s in np.argsort(-lp[i]) if frets[i, s] != INVALID][:6] for i in idx]
    opts = []
    for combo in itertools.product(*choices):
        if len(set(combo)) < len(combo):
            continue
        score = sum(lp[i, s] for i, s in zip(idx, combo))
        fr = [frets[i, s] for i, s in zip(idx, combo) if frets[i, s] > 0]
        if fr:
            score -= span_cost * max(0, max(fr) - min(fr) - 3) + pos_cost * min(fr)
        opts.append((score, combo, min(fr) if fr else None))
    if not opts:  # more notes than strings: fall back to the argmax
        combo = tuple(int(np.argmax(lp[i])) for i in idx)
        opts = [(sum(lp[i, s] for i, s in zip(idx, combo)), combo, None)]
    opts.sort(key=lambda o: -o[0])
    return opts[:top]


def decode(logits, frets, cid, span_cost=0.5, move_cost=0.0, pos_cost=0.0, top=8):
    """String index per note: distinct strings inside each chord, a stretch
    cost per fret beyond a 4-fret span, and with `move_cost` > 0 a Viterbi
    over chords adding `move_cost` per fret of hand movement (open-only
    chords carry the previous position)."""
    lp = log_softmax_valid(logits, frets)
    n = len(lp)
    out = np.zeros(n, np.int64)
    groups = np.split(np.arange(n), np.flatnonzero(np.diff(cid)) + 1) if n else []
    if move_cost <= 0:
        for g in groups:
            out[g] = chord_options(lp, frets, g, span_cost, 1, pos_cost)[0][1]
        return out
    # Viterbi: state = option of the chord; position carried through opens.
    prev = None  # list of (total, position, backpointer path index)
    back = []
    for g in groups:
        opts = chord_options(lp, frets, g, span_cost, top, pos_cost)
        cur = []
        for score, combo, pos in opts:
            if prev is None:
                cur.append((score, pos, -1))
                continue
            best = None
            for k, (tot, ppos, _) in enumerate(prev):
                move = abs(pos - ppos) if pos is not None and ppos is not None else 0
                c = tot + score - move_cost * move
                if best is None or c > best[0]:
                    best = (c, pos if pos is not None else ppos, k)
            cur.append(best)
        back.append((g, opts, [c[2] for c in cur]))
        prev = cur
    k = int(np.argmax([c[0] for c in prev]))
    for g, opts, bp in reversed(back):
        out[g] = opts[k][1]
        k = bp[k]
    return out


# ---------------------------------------------------------------- playability

def playability(notes, strings, frets_of):
    """(chords, chords with a stretch > 5 frets, chords with two notes on one
    string) for notes in `sort_notes` order; `frets_of[i]` is the fret
    (or < 0 when the note got no position)."""
    on = [x[0] for x in notes]
    cid = chord_ids(on)
    chords = stretch = clash = 0
    for c in np.unique(cid):
        idx = np.flatnonzero(cid == c)
        if len(idx) < 2:
            continue
        chords += 1
        st = [strings[i] for i in idx if frets_of[i] >= 0]
        fr = [frets_of[i] for i in idx if frets_of[i] > 0]
        if fr and max(fr) - min(fr) > 5:
            stretch += 1
        if len(set(st)) < len(st):
            clash += 1
    return chords, stretch, clash
