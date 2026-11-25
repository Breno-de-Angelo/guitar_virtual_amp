"""Real-song benchmark from (recording, Guitar Pro tab) pairs: aligns each
tab to its recording, writes the tab's guitar notes in recording time as
labels, and a copy of the tab with a `.gvtab.json` sidecar so the app's Tabs
page plays the recording in sync with the cursor for hand-checking.

Alignment (synctoolbox, Mueller et al. 2021): the tab's notes (every pitched
track, drums left out) become symbolic pitch features (df_to_pitch_features,
df_to_pitch_onset_features); the recording gets the matching audio features
(IIR pitch filterbank, tuning estimated); the best chroma transposition is
found on 1 Hz CENS (a tab notated in E standard against a recording tuned a
half step down: the labels are transposed with it, up to --max-transpose);
then MrMsDTW on quantized chroma plus DLNCO onset features at 50 Hz, and
note onsets and offsets are read off the warping path. GP8 sync points are
not used: they refer to the file's embedded audio, not to the recording.

Rejection (thresholds chosen on EGSet12/GOAT with validate_tab_align.py,
see README): `cost_ratio`, mean chroma cosine distance along the path over
the mean distance of random frame pairs (1 = no better than chance), and
`stall`, the share of the tab's note span where the local tempo along the
path (2 s windows, in score time and in audio time) is off the song's
median by more than 2x (a section one side has and the other doesn't).
Pairs with cost_ratio > --max-cost (0.70) or stall > --max-stall (0.10)
are rejected. `worst_window` (cost_ratio of the worst 10 s) and
`shift_contrast` are reported for hand-checking but not used.

Pairs TSV (one per line, `#` comments, paths relative to the TSV):
    audio <TAB> tab [<TAB> id [<TAB> track indices, comma-separated]]
`id` defaults to the tab's file stem; tracks default to every guitar track.

Output (`--out DIR`):
    DIR/pairs/<id>/notes/<k>_<track>.txt  aligned notes per guitar track
    DIR/pairs/<id>/notes/all_guitars.txt  union of the guitar tracks
    DIR/pairs/<id>/report.json            stats, verdict, warnings
    DIR/check/<id>.<gp5|gp|...>           copy of the tab (repeats written out)
    DIR/check/<id>.gvtab.json             TabMetadata sidecar: recording path, bar_audio_secs
    DIR/alignment_report.tsv              one line per pair
    DIR/checklist.txt                     `<status> <id>  # auto verdict`, edited by hand:
                                          todo / checked / rejected
Notes files: `onset offset pitch velocity technique string fret` (seconds,
MIDI, GP velocity, none/bend/slide/vibrato/legato/harmonic/palm, string 1 =
highest): the first five columns are what eval.rs `eval_manifest` and
bench_notes.py read.

Usage:
    python align_tabs.py align --pairs pairs.tsv --out ~/gva_real [--force]
    python align_tabs.py manifest --out ~/gva_real
        -> eval_real.tsv (checked, union of guitars, group real),
           eval_real_tracks.tsv (checked, one row per guitar track, group real/guitar<k>),
           train_real_unchecked.tsv (accepted but not yet checked)
    python align_tabs.py score-tab --out ~/gva_real --bench BENCH --fingering-bin target/release/examples/fingering
"""
import argparse
import contextlib
import hashlib
import io
import json
import os
import pickle
import re
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gp_tabs  # noqa: E402

FS = 22050
RATE = 50  # feature frames per second
# Shift of the audio onset peaks. synctoolbox's default, -25 ms, puts the
# aligned notes ~20 ms early against EGSet12/GOAT labels; 0 removes the bias.
ONSET_OFFSET_MS = 0.0
STEP_WEIGHTS = np.array([1.5, 1.5, 2.0])
# Rejection thresholds, chosen on the validation sets (validate_tab_align.py).
MAX_COST = 0.70
MAX_STALL = 0.10
AUDIO_EXTS = (".wav", ".flac", ".mp3", ".ogg", ".m4a", ".aac")


# ---------------------------------------------------------------- features

def load_audio(path):
    import librosa

    x, _ = librosa.load(str(path), sr=FS, mono=True)
    return x.astype(np.float64)


def audio_features(x):
    from synctoolbox.feature.chroma import pitch_to_chroma, quantize_chroma
    from synctoolbox.feature.dlnco import pitch_onset_features_to_DLNCO
    from synctoolbox.feature.pitch import audio_to_pitch_features
    from synctoolbox.feature.pitch_onset import audio_to_pitch_onset_features
    from synctoolbox.feature.utils import estimate_tuning

    tuning = int(estimate_tuning(x, FS))
    with contextlib.redirect_stdout(io.StringIO()):  # synctoolbox prints progress dots
        f_pitch = audio_to_pitch_features(f_audio=x, Fs=FS, tuning_offset=tuning, feature_rate=RATE)
        peaks = audio_to_pitch_onset_features(f_audio=x, Fs=FS, tuning_offset=tuning, manual_offset=ONSET_OFFSET_MS)
    chroma = quantize_chroma(pitch_to_chroma(f_pitch=f_pitch))
    dlnco = pitch_onset_features_to_DLNCO(f_peaks=peaks, feature_rate=RATE, feature_sequence_length=chroma.shape[1])
    return {"chroma": chroma, "dlnco": dlnco, "peaks": peaks, "tuning": tuning, "duration": len(x) / FS}


def cached_audio_features(path, cache_dir):
    path = Path(path)
    st = path.stat()
    key = hashlib.sha1(f"{path.resolve()}|{st.st_size}|{st.st_mtime_ns}|{FS}|{RATE}|{ONSET_OFFSET_MS}".encode()).hexdigest()[:16]
    f = Path(cache_dir) / f"{key}.pkl" if cache_dir else None
    if f is not None and f.exists():
        return pickle.load(open(f, "rb"))
    feats = audio_features(load_audio(path))
    if f is not None:
        f.parent.mkdir(parents=True, exist_ok=True)
        pickle.dump(feats, open(f, "wb"))
    return feats


def symbolic_features(notes, n_frames):
    """Chroma + DLNCO of tab notes [(start, end, pitch, velocity)] in score time."""
    import pandas as pd
    from synctoolbox.feature.chroma import pitch_to_chroma, quantize_chroma
    from synctoolbox.feature.csv_tools import df_to_pitch_features, df_to_pitch_onset_features
    from synctoolbox.feature.dlnco import pitch_onset_features_to_DLNCO

    rows = [(s, max(e - s, 0.02), p, max(v, 1)) for s, e, p, v in notes if 21 <= p <= 108]
    # A silent anchor note at the end pins the feature length to the tab's.
    end = n_frames / RATE
    df = pd.DataFrame(rows, columns=["start", "duration", "pitch", "velocity"])
    df["instrument"] = "guitar"
    pad = pd.DataFrame([(end - 0.02, 0.02, 21, 1e-6, "guitar")], columns=df.columns)
    df = pd.concat([df, pad], ignore_index=True)
    f_pitch = df_to_pitch_features(df, feature_rate=RATE)
    chroma = quantize_chroma(pitch_to_chroma(f_pitch=f_pitch))
    peaks = df_to_pitch_onset_features(df[df["velocity"] >= 1])
    dlnco = pitch_onset_features_to_DLNCO(f_peaks=peaks, feature_rate=RATE, feature_sequence_length=chroma.shape[1])
    return chroma, dlnco


def alignment_notes(tab, tracks=None):
    """Notes of every pitched track (all the tab knows about the recording)."""
    out = []
    for i, tr in enumerate(tab.tracks):
        if tr.kind == "drums" or (tracks is not None and i not in tracks):
            continue
        out += [(n["start"], n["end"], n["pitch"], n["velocity"]) for n in tab.notes(i)]
    return out


# ---------------------------------------------------------------- alignment

def _unit(f):
    from synctoolbox.feature.utils import normalize_feature

    return normalize_feature(f, 2, 0.001)


def align(sym_notes, score_secs, audio, max_transpose=2):
    """Warping path between score time and audio time, with path statistics.
    Returns a dict: wp (2 x T, frames), transpose (semitones, audio minus
    tab), stats."""
    from synctoolbox.dtw.mrmsdtw import sync_via_mrmsdtw
    from synctoolbox.dtw.utils import make_path_strictly_monotonic
    from synctoolbox.feature.chroma import quantized_chroma_to_CENS
    from synctoolbox.feature.utils import shift_chroma_vectors

    n_frames = int(np.ceil(score_secs * RATE)) + 1
    s_chroma, s_dlnco = symbolic_features(sym_notes, n_frames)
    a_chroma, a_dlnco = audio["chroma"], audio["dlnco"]
    cens_s = quantized_chroma_to_CENS(s_chroma, 201, 50, RATE)[0]
    cens_a = quantized_chroma_to_CENS(a_chroma, 201, 50, RATE)[0]
    shift, contrast = chroma_shift(cens_a, cens_s)
    transpose = (shift + 6) % 12 - 6  # roll by `shift` = tab pitches + shift
    if abs(transpose) > max_transpose:
        shift, transpose_used = 0, 0
    else:
        transpose_used = transpose
    s_chroma = shift_chroma_vectors(s_chroma, shift)
    s_dlnco = shift_chroma_vectors(s_dlnco, shift)
    wp = sync_via_mrmsdtw(f_chroma1=s_chroma, f_onset1=s_dlnco, f_chroma2=a_chroma, f_onset2=a_dlnco,
                          input_feature_rate=RATE, step_weights=STEP_WEIGHTS, threshold_rec=10 ** 6, verbose=False)
    wp = make_path_strictly_monotonic(wp).astype(np.float64)
    span = (min(n[0] for n in sym_notes), max(n[1] for n in sym_notes)) if sym_notes else (0.0, score_secs)
    stats = path_stats(wp, s_chroma, a_chroma, s_dlnco, a_dlnco, span)
    stats["transpose_found"] = transpose
    stats["shift_contrast"] = contrast
    return {"wp": wp, "transpose": transpose_used, "stats": stats, "tuning_cents": audio["tuning"]}


def chroma_shift(cens_a, cens_s):
    """Best chroma roll of the tab (as synctoolbox's
    compute_optimal_chroma_shift: DTW cost of 1 Hz CENS for each of the 12),
    and its cost over the median cost of the other 11 (low = distinct)."""
    import librosa

    costs = []
    for k in range(12):
        D, w = librosa.sequence.dtw(X=cens_a, Y=np.roll(cens_s, k, axis=0), metric="cosine")
        costs.append(D[-1, -1] / len(w))
    best = int(np.argmin(costs))
    return best, float(costs[best] / max(np.median(np.delete(costs, best)), 1e-9))


def _stall(x, y, lo, hi):
    """Share of [lo, hi] of x where the local slope dy/dx over 2 s windows
    is off its median by more than 2x (x, y increasing along the path).
    Runs touching either end are left out: an intro or ending one side
    doesn't have is not a structure difference."""
    grid = np.arange(lo, max(lo, hi - 2.0), 0.5)
    if len(grid) == 0:
        return 0.0
    slopes = (np.interp(grid + 2.0, x, y) - np.interp(grid, x, y)) / 2.0
    med = float(np.median(slopes))
    if med <= 0:
        return 1.0
    off = (slopes < med / 2) | (slopes > med * 2)
    inner = np.nonzero(~off)[0]
    if len(inner) == 0:
        return 1.0
    return float(np.sum(off[inner[0]:inner[-1] + 1]) / len(off))


def path_stats(wp, s_chroma, a_chroma, s_dlnco=None, a_dlnco=None, span=None, seed=0):
    su, au = _unit(s_chroma), _unit(a_chroma)
    active = s_chroma.sum(0) > 1e-6  # tab frames with sounding notes
    i, j = wp[0].astype(int), wp[1].astype(int)
    i, j = np.clip(i, 0, su.shape[1] - 1), np.clip(j, 0, au.shape[1] - 1)
    d = 1 - np.sum(su[:, i] * au[:, j], 0)
    on = active[i]
    rng = np.random.default_rng(seed)
    act_idx = np.nonzero(active)[0]
    if len(act_idx) == 0 or not on.any():
        nan = float("nan")
        return {"cost": 1.0, "background": 1.0, "cost_ratio": 1.0, "worst_window": 1.0, "stall": 1.0,
                "score_stall": 1.0, "audio_stall": 1.0, "onset_ratio": nan, "tempo_ratio": nan,
                "linear_rate": nan, "linear_offset": nan, "linear_resid_p90": nan}
    ri = rng.choice(act_idx, 20000)
    rj = rng.integers(0, au.shape[1], 20000)
    background = float(np.mean(1 - np.sum(su[:, ri] * au[:, rj], 0)))
    cost = float(np.mean(d[on]))
    # Worst 10 s window of score time (windows with at least 3 s of notes).
    win, worst, worst_at = 10 * RATE, 0.0, 0
    for start in range(0, max(1, int(i.max()) - win // 2), win // 2):
        m = on & (i >= start) & (i < start + win)
        if m.sum() >= 3 * RATE and float(np.mean(d[m])) > worst:
            worst, worst_at = float(np.mean(d[m])), start
    worst = worst or cost
    # Local tempo along the path inside the tab's notes, 2 s windows of score
    # time (the recording skips part of the tab) and of audio time (the
    # recording has a part the tab doesn't).
    t_s, t_a = wp[0] / RATE, wp[1] / RATE
    lo, hi = span if span is not None else (t_s[0], t_s[-1])
    score_stall = _stall(t_s, t_a, lo, hi)
    audio_stall = _stall(t_a, t_s, *np.interp([lo, hi], t_s, t_a))
    stall = max(score_stall, audio_stall)
    grid = np.arange(lo, max(lo, hi - 2.0), 0.5)
    slopes = (np.interp(grid + 2.0, t_s, t_a) - np.interp(grid, t_s, t_a)) / 2.0
    med = float(np.median(slopes)) if len(slopes) else float("nan")
    onset_ratio = float("nan")
    if s_dlnco is not None:
        so, ao = _unit(s_dlnco), _unit(a_dlnco)
        hit = s_dlnco.sum(0) > 1e-6
        ii, jj = i[hit[i]], j[hit[i]]
        hit_idx = np.nonzero(hit)[0]
        if len(ii) and len(hit_idx):
            d_on = 1 - np.sum(so[:, ii] * ao[:, jj], 0)
            ri2 = rng.choice(hit_idx, 20000)
            bg_on = np.mean(1 - np.sum(so[:, ri2] * ao[:, rng.integers(0, ao.shape[1], 20000)], 0))
            onset_ratio = float(np.mean(d_on) / max(bg_on, 1e-9))
    # Linear fit of the path (does the recording follow the tab's tempo map?).
    A = np.stack([t_s, np.ones_like(t_s)], 1)
    coef, *_ = np.linalg.lstsq(A, t_a, rcond=None)
    resid = np.abs(t_a - A @ coef)
    return {"cost": cost, "background": background, "cost_ratio": cost / max(background, 1e-9),
            "worst_window": worst / max(background, 1e-9), "worst_window_secs": worst_at / RATE,
            "stall": stall, "score_stall": score_stall,
            "audio_stall": audio_stall, "onset_ratio": onset_ratio,
            "tempo_ratio": 1.0 / med if med > 0 else float("nan"),
            "linear_rate": float(coef[0]), "linear_offset": float(coef[1]),
            "linear_resid_p90": float(np.percentile(resid, 90))}


def score_to_audio(wp, t):
    return np.interp(np.asarray(t, float) * RATE, wp[0], wp[1]) / RATE


def aligned_track_notes(tab, track_index, al):
    """The track's notes in recording time (dicts as gp_tabs.Tab.notes)."""
    notes = tab.notes(track_index)
    if not notes:
        return []
    on = score_to_audio(al["wp"], [n["start"] for n in notes])
    off = score_to_audio(al["wp"], [n["end"] for n in notes])
    out = [dict(n, start=float(a), end=float(max(b, a + 0.01)), pitch=int(n["pitch"] + al["transpose"]))
           for n, a, b in zip(notes, on, off)]
    return sorted(out, key=lambda n: (n["start"], n["pitch"]))


def worst_bars(occ, start):
    """1-based bar numbers (of the checked copy, one bar per pass) of the
    10 s of score time with the worst fit."""
    ks = [k + 1 for k, o in enumerate(occ) if o[2] > start and o[1] < start + 10]
    return f"{ks[0]}-{ks[-1]}" if ks else ""


def verdict(stats, transpose_found, max_transpose, limits):
    reasons = []
    if stats["cost_ratio"] > limits["cost"]:
        reasons.append(f"cost_ratio {stats['cost_ratio']:.2f} > {limits['cost']}")
    if stats["stall"] > limits["stall"]:
        reasons.append(f"stall {stats['stall']:.2f} > {limits['stall']}")
    if abs(transpose_found) > max_transpose:
        reasons.append(f"best transposition {transpose_found:+d} semitones > {max_transpose}")
    return ("rejected" if reasons else "accepted"), reasons


# ---------------------------------------------------------------- outputs

def write_notes(path, notes):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        for n in notes:
            f.write(f"{n['start']:.4f} {n['end']:.4f} {n['pitch']} {n['velocity']} {n['technique']} {n['string']} {n['fret']}\n")


def safe_id(text):
    return re.sub(r"[^A-Za-z0-9_-]+", "_", text).strip("_") or "pair"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def write_check_copy(tab, al, audio_path, audio_dur, check_dir, pid, tracks_info):
    """Copy of the tab (repeats written out) + `.gvtab.json` with the bar
    times in the recording (repository.rs `TabMetadata`)."""
    check_dir.mkdir(parents=True, exist_ok=True)
    order = tab.order()
    warnings = []
    unrolled = order != list(range(len(tab.bars)))
    if unrolled or tab.backing_audio or tab.sync_points:
        try:
            copy_path = gp_tabs.write_unrolled(tab, check_dir / pid)
            if unrolled:
                warnings.append("repeats written out in the copy (one bar per pass)")
            per_bar = True
        except Exception as e:  # keep the original file; sync only on first passes
            copy_path = check_dir / f"{pid}{tab.path.suffix}"
            copy_path.write_bytes(tab.path.read_bytes())
            warnings.append(f"copy not unrolled ({e}); cursor is interpolated inside repeated passes")
            per_bar = not unrolled
    else:
        copy_path = check_dir / f"{pid}{tab.path.suffix}"
        copy_path.write_bytes(tab.path.read_bytes())
        per_bar = True
    occ = tab.occurrences()
    if per_bar:
        score_starts = [o[1] for o in occ] + [occ[-1][2]]
    else:
        first = {}
        for b, s, _, _ in occ:
            first.setdefault(b, s)
        score_starts = [first.get(b, np.nan) for b in range(len(tab.bars))]
    bar_secs = list(score_to_audio(al["wp"], np.nan_to_num(score_starts, nan=0.0)))
    # Strictly increasing (the app interpolates between them).
    for k in range(1, len(bar_secs)):
        bar_secs[k] = max(bar_secs[k], bar_secs[k - 1] + 0.001)
    st0 = occ[0][3][0][2] if occ else tab.initial_tempo
    meta = {
        "version": 1,
        "title": tab.title or pid,
        "artist": tab.artist,
        "origin": "import",
        "created_unix": int(time.time()),
        "source": {"file_name": Path(audio_path).name, "path": str(Path(audio_path).resolve()),
                   "sha256": sha256(audio_path), "duration_secs": round(audio_dur, 3), "youtube_id": None},
        "separation": "",
        "transcription": "align_tabs.py: tab aligned with synctoolbox MrMsDTW",
        "app_version": "",
        "tracks": tracks_info,
        "tempo_bpm": float(st0) * (al["stats"].get("tempo_ratio") or 1.0),
        "bar_audio_secs": [round(float(v), 4) for v in bar_secs],
        "spotify_track_id": None,
    }
    sidecar = copy_path.with_suffix(".gvtab.json")
    json.dump(meta, open(sidecar, "w"), indent=1, ensure_ascii=False)
    return copy_path, sidecar, warnings


def read_pairs(path):
    base = Path(path).resolve().parent
    out = []
    for line in open(path):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        cols = line.rstrip("\n").split("\t")
        if len(cols) < 2:
            raise SystemExit(f"{path}: expected `audio<TAB>tab[<TAB>id[<TAB>tracks]]`: {line!r}")
        audio, tab = (Path(os.path.expanduser(c)) for c in cols[:2])
        audio, tab = (p if p.is_absolute() else base / p for p in (audio, tab))
        pid = safe_id(cols[2]) if len(cols) > 2 and cols[2].strip() else safe_id(tab.stem)
        tracks = [int(v) for v in cols[3].split(",")] if len(cols) > 3 and cols[3].strip() else None
        out.append({"audio": audio, "tab": tab, "id": pid, "tracks": tracks})
    ids = [p["id"] for p in out]
    dup = {i for i in ids if ids.count(i) > 1}
    if dup:
        raise SystemExit(f"duplicate ids in {path}: {sorted(dup)} (set the 3rd column)")
    return out


def process_pair(pair, out, args):
    pid = pair["id"]
    pdir = out / "pairs" / pid
    tab = gp_tabs.load(pair["tab"])
    occ = tab.occurrences()
    if not occ:
        raise ValueError("tab has no bars")
    audio = cached_audio_features(pair["audio"], out / "cache")
    t0 = time.time()
    al = align(alignment_notes(tab), occ[-1][2], audio, args.max_transpose)
    stats = al["stats"]
    limits = {"cost": args.max_cost, "stall": args.max_stall}
    status, reasons = verdict(stats, stats["transpose_found"], args.max_transpose, limits)
    guitars = pair["tracks"] if pair["tracks"] is not None else [i for i, t in enumerate(tab.tracks) if t.kind == "guitar"]
    warnings = []
    if not guitars:
        warnings.append("no guitar track found (set the track column)")
    if al["transpose"]:
        warnings.append(f"recording is {al['transpose']:+d} semitones from the tab: labels transposed")
    if tab.sync_points:
        warnings.append("tab has GP8 sync points (ignored: they refer to its embedded audio)")
    tracks_out, tracks_info, union = [], [], []
    for k, ti in enumerate(guitars):
        tr = tab.tracks[ti]
        notes = aligned_track_notes(tab, ti, al)
        f = pdir / "notes" / f"{k + 1}_{safe_id(tr.name) or 'track'}.txt"
        write_notes(f, notes)
        union += notes
        tracks_out.append({"index": ti, "name": tr.name, "kind": tr.kind, "notes": len(notes), "file": str(f),
                           "tuning": tr.tuning, "capo": tr.capo})
    for ti, tr in enumerate(tab.tracks):
        tracks_info.append({"name": tr.name, "stem": "mix", "tuning_name": " ".join(map(str, tr.tuning)),
                            "notes": len(tab.notes(ti)) if tr.kind != "drums" else 0})
    union.sort(key=lambda n: (n["start"], n["pitch"]))
    dedup = []
    for n in union:  # same pitch struck within 30 ms by two tracks: one note
        if any(m["pitch"] == n["pitch"] and abs(m["start"] - n["start"]) < 0.03 for m in dedup[-12:]):
            continue
        dedup.append(n)
    write_notes(pdir / "notes" / "all_guitars.txt", dedup)
    copy_path, sidecar, w = write_check_copy(tab, al, pair["audio"], audio["duration"], out / "check", pid,
                                             tracks_info)
    warnings += w
    report = {
        "id": pid, "audio": str(pair["audio"]), "tab": str(pair["tab"]), "status": status, "reasons": reasons,
        "warnings": warnings, "stats": stats, "transpose": al["transpose"], "tuning_cents": al["tuning_cents"],
        "audio_secs": audio["duration"], "tab_secs": occ[-1][2],
        "bars": len(tab.bars), "occurrences": len(occ), "tracks": tracks_out,
        "worst_bars": worst_bars(occ, stats.get("worst_window_secs", 0.0)),
        "all_guitars": str(pdir / "notes" / "all_guitars.txt"), "copy": str(copy_path), "sidecar": str(sidecar),
        "align_secs": time.time() - t0,
    }
    json.dump(report, open(pdir / "report.json", "w"), indent=1)
    return report


# ---------------------------------------------------------------- checklist

STATUSES = ("todo", "checked", "rejected")


def read_checklist(path):
    out = {}
    if path.exists():
        for line in open(path):
            body = line.split("#", 1)[0].split()
            if len(body) >= 2 and body[0] in STATUSES:
                out[body[1]] = body[0]
    return out


def write_checklist(path, reports, statuses):
    lines = ["# Hand-check each pair in the app (Tabs page: open check/<id>.* and press play),",
             "# then set the first word: todo, checked (labels are right) or rejected.",
             "# `align_tabs.py manifest` puts only `checked` pairs in the test manifest.", ""]
    for r in sorted(reports, key=lambda r: r["id"]):
        st = r["stats"]
        auto = r["status"] + (f" ({'; '.join(r['reasons'])})" if r["reasons"] else "")
        lines.append(f"{statuses.get(r['id'], 'todo'):8s} {r['id']}  # auto: {auto}; cost {st['cost_ratio']:.2f}, "
                     f"stall {st['stall']:.2f}, worst {st['worst_window']:.2f} (bars {r.get('worst_bars', '')}), "
                     f"tempo x{st['tempo_ratio']:.2f}"
                     + (f"; {'; '.join(r['warnings'])}" if r["warnings"] else "") + f"; open {Path(r['copy']).name}")
    path.write_text("\n".join(lines) + "\n")


def load_reports(out):
    return [json.load(open(f)) for f in sorted((out / "pairs").glob("*/report.json"))]


# ---------------------------------------------------------------- commands

def cmd_align(args):
    out = args.out.expanduser()
    out.mkdir(parents=True, exist_ok=True)
    pairs = read_pairs(args.pairs.expanduser())
    statuses = read_checklist(out / "checklist.txt")
    for k, pair in enumerate(pairs):
        rep = out / "pairs" / pair["id"] / "report.json"
        if rep.exists() and not args.force:
            print(f"[{k + 1}/{len(pairs)}] {pair['id']}: already aligned (--force to redo)")
            continue
        try:
            r = process_pair(pair, out, args)
        except Exception as e:
            print(f"[{k + 1}/{len(pairs)}] {pair['id']}: ERROR {e!r}", file=sys.stderr)
            continue
        if args.force and statuses.get(pair["id"]) == "checked":
            statuses[pair["id"]] = "todo"  # a new alignment needs a new check
        st = r["stats"]
        print(f"[{k + 1}/{len(pairs)}] {pair['id']}: {r['status']} cost {st['cost_ratio']:.2f} stall "
              f"{st['stall']:.2f} worst {st['worst_window']:.2f} tempo x{st['tempo_ratio']:.2f} "
              f"transpose {r['transpose']:+d} {' '.join(r['reasons'])}", flush=True)
    reports = load_reports(out)
    with open(out / "alignment_report.tsv", "w") as f:
        f.write("id\tstatus\tcost_ratio\tstall\tworst_window\tworst_bars\ttempo_ratio\ttranspose\tguitar_tracks\t"
                "reasons\twarnings\n")
        for r in reports:
            st = r["stats"]
            f.write(f"{r['id']}\t{r['status']}\t{st['cost_ratio']:.3f}\t{st['stall']:.3f}\t{st['worst_window']:.3f}\t"
                    f"{r.get('worst_bars', '')}\t{st['tempo_ratio']:.3f}\t{r['transpose']}\t{len(r['tracks'])}\t"
                    f"{'; '.join(r['reasons'])}\t"
                    f"{'; '.join(r['warnings'])}\n")
    write_checklist(out / "checklist.txt", reports, statuses)
    print(f"{len(reports)} pairs: {out / 'alignment_report.tsv'}, checklist {out / 'checklist.txt'}")


def cmd_manifest(args):
    out = args.out.expanduser()
    statuses = read_checklist(out / "checklist.txt")
    reports = load_reports(out)
    test, tracks, unchecked = [], [], []
    for r in reports:
        s = statuses.get(r["id"], "todo")
        if s == "checked":
            if r["status"] == "rejected":
                print(f"{r['id']}: checked by hand although the aligner rejected it ({'; '.join(r['reasons'])})")
            test.append(f"{r['audio']}\t{r['all_guitars']}\treal\t\n")
            for k, t in enumerate(r["tracks"]):
                tracks.append(f"{r['audio']}\t{t['file']}\treal/guitar{k + 1}\t\n")
        elif s == "todo" and r["status"] == "accepted":
            unchecked.append(f"{r['audio']}\t{r['all_guitars']}\treal-unchecked\t\n")
    for name, rows in (("eval_real.tsv", test), ("eval_real_tracks.tsv", tracks), ("train_real_unchecked.tsv", unchecked)):
        (out / name).write_text("".join(rows))
        print(f"{out / name}: {len(rows)} rows")


def read_tab_notes(path):
    out = []
    for line in open(path):
        v = line.split()
        if len(v) >= 6:
            out.append((float(v[0]), float(v[1]), int(v[2]), int(v[5])))
    return out


def cmd_score_tab(args):
    """String accuracy of the app's fingering on the true notes, and Tab F1
    of cached transcriptions (bench_notes.py) placed by it, on the checked
    pairs' guitar tracks (bench_tab.py's metrics)."""
    from bench_notes import items
    from bench_tab import f1_match, viterbi

    out = args.out.expanduser()
    rows = {}
    its = items(out, None, 0, [args.manifest])
    truths = [read_tab_notes(it["notes"]) for it in its]
    vit = viterbi(args.fingering_bin.expanduser(), [[(n[0], n[2]) for n in t] for t in truths])
    for it, t, v in zip(its, truths, vit):
        acc = float(np.mean([s == n[3] for (s, _), n in zip(v, t)])) if t else 0.0
        rows.setdefault("fingering: viterbi (true notes)", {}).setdefault(it["group"], []).append({"string_acc": acc})
    for eng in [e for e in args.notes_engines.split(",") if e]:
        ok, ests = [], []
        for it, t in zip(its, truths):
            f = args.bench.expanduser() / "notes" / eng / f"{it['id']}.json"
            if f.exists():
                ests.append([(n[0], n[1], n[2]) for n in json.load(open(f)) if len(n) < 4 or "guitar" in n[3]])
                ok.append((it, t))
        placed = viterbi(args.fingering_bin.expanduser(), [[(n[0], n[2]) for n in e] for e in ests]) if ests else []
        for (it, t), e, pl in zip(ok, ests, placed):
            est = [(a, b, p, s) for (a, b, p), (s, _) in zip(e, pl) if s > 0]
            rows.setdefault(f"modular: {eng} + viterbi", {}).setdefault(it["group"], []).append(
                {"f50": f1_match(t, est, lambda n: 0), "tab_f1": f1_match(t, est, lambda n: n[3])})
    for method, groups in rows.items():
        for g, ms in sorted(groups.items()):
            cells = ", ".join(f"{k} {np.mean([m[k] for m in ms]):.3f}" for k in ms[0])
            print(f"{method} | {g} ({len(ms)}): {cells}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("align")
    a.add_argument("--pairs", type=Path, required=True)
    a.add_argument("--out", type=Path, required=True)
    a.add_argument("--max-transpose", type=int, default=2)
    a.add_argument("--max-cost", type=float, default=MAX_COST)
    a.add_argument("--max-stall", type=float, default=MAX_STALL)
    a.add_argument("--force", action="store_true", help="realign pairs that already have a report")
    m = sub.add_parser("manifest")
    m.add_argument("--out", type=Path, required=True)
    s = sub.add_parser("score-tab")
    s.add_argument("--out", type=Path, required=True)
    s.add_argument("--manifest", default="eval_real.tsv", help="manifest in --out, as given to bench_notes.py")
    s.add_argument("--bench", type=Path, required=True, help="bench_notes.py --out folder (cached notes)")
    s.add_argument("--notes-engines", default="")
    s.add_argument("--fingering-bin", type=Path, required=True)
    args = ap.parse_args()
    {"align": cmd_align, "manifest": cmd_manifest, "score-tab": cmd_score_tab}[args.cmd](args)


if __name__ == "__main__":
    main()
