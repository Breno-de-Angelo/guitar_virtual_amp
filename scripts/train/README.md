# Training and benchmark data for the guitar transcription model

Fine-tunes the guitar CRNN (Kong et al. 2021 architecture, GAPS weights of Riley et al. 2024)
on electric guitar in many tones, and prepares the labelled recordings the Rust benchmarks in
`guitar_core/src/transcribe/eval.rs` read. Needs a CUDA GPU with 8 GB, Python 3.12 with
`torch numpy soundfile mido onnx scipy pyguitarpro`, and ffmpeg.

## Datasets

Put them under one root (here `~/.cache/gva_test`). None of them is in the repository.

| Folder | Dataset | License | Use |
|---|---|---|---|
| `egdb/` | EGDB (Chen et al., ICASSP 2022), Google Drive | none stated ("feel free to use") | train (ids not multiple of 5), test (multiples of 5, real clips) |
| `guitar_techs/` | Guitar-TECHS (Pedroza et al., ICASSP 2025), Zenodo 14963133 | CC BY 4.0 | chords/scales train; music excerpts and techniques test |
| `idmt/` | IDMT-SMT-Guitar (Kehling et al., DAFx 2014), Zenodo 7544110 | CC BY-NC-ND 4.0 | test only (per-note techniques) |
| `goat/GOAT/` | GOAT (Loth et al., ISMIR 2025), on request | non-commercial | official test split; never in a shipped model |
| `guitarset/` | GuitarSet (Xi et al., ISMIR 2018), Zenodo 3371780 | CC BY 4.0 | players 00-02 train (`--guitarset`), 03-05 test |
| `musdb/musdb18/` | MUSDB18 (Rafii et al. 2017), Zenodo 1117372 | non-commercial research | drums/bass/vocals for test band mixes |

## Steps

```bash
PY=python  # the training venv
# 1. Amp renders of the training DI files through the app's presets (Metal High Gain
#    and Blues Crunch are left out: they are the distortion benchmark).
#    Link EGDB DI as <id>.wav and Guitar-TECHS DI as <part>__<file>.wav first.
cargo run --release -p guitar_desktop --bin render_tones -- --out renders/egdb --rate 16000 \
    --drive 0.7,1.3 --preset "Classic Rock Rhythm" --preset "Doom Sludge" --preset "Metal Djent" \
    --preset "Rock Lead" --preset "Rockabilly Slapback" --preset "Shoegaze Wall" di/egdb
# 2. Notes files, training list and eval manifests.
$PY prepare_data.py --root ~/.cache/gva_test --out ~/.cache/gva_test/prepared_gs \
    --renders ~/.cache/gva_test/renders --guitarset [--goat ~/.cache/gva_test/goat/GOAT]
# 3. Remove constant label latency (MIDI pickups, annotation conventions).
$PY align_labels.py --data ~/.cache/gva_test/prepared_gs --init guitar-gaps.pth \
    --manifest eval_techs.tsv --manifest eval_techs_techniques.tsv --manifest eval_idmt.tsv
# 4. Band mixes for testing: labelled guitar over MUSDB18 drums, bass and vocals.
$PY make_mixes.py --musdb ~/.cache/gva_test/musdb/musdb18 --manifest prepared/eval_techs.tsv \
    --manifest prepared/eval_egdb.tsv --out prepared --groups music-micamp,egdb/Marshall,egdb/real
# 5. Fine-tune (~1.3 s/step on an RTX 4070 Laptop; best checkpoint by EGDB onset F1).
$PY finetune_crnn.py --data ~/.cache/gva_test/prepared_gs --init guitar-gaps.pth \
    --out runs/electric --steps 8000
# 6. Benchmark the exported model.
GVA_GUITAR_MODEL=runs/electric/model.onnx GVA_EVAL_MANIFEST=prepared/eval_idmt_tech.tsv \
    cargo test --release --workspace --exclude guitar_android eval_manifest -- --ignored --nocapture
```

## Notes

- Mixed precision doesn't help: fp16 overflows in the model's power spectrum and the GRUs have
  no fast bf16 path, so training runs in fp32 with TF32 matmuls, 4 x 4 s crops, 2 batches per
  step, BatchNorm frozen.
- Guitar-TECHS labels come from a hexaphonic MIDI pickup and are 25-40 ms late; IDMT marks
  onsets ~30 ms early; GOAT's alignment is off by up to ±40 ms per item. `align_labels.py`
  shifts each recording by the median offset against the original model's onsets. Without
  it, a fine-tuned model learns the lag.
- Licenses decide what can ship: a model trained on GOAT, IDMT or MUSDB can't. The model
  trained by these steps without `--goat` uses EGDB (no license stated), Guitar-TECHS and
  GuitarSet (CC BY 4.0) and the GAPS weights (MIT); ask EGDB's authors before distributing it.

## Real-song benchmark: aligning your own tabs to your own recordings

`align_tabs.py` turns (recording, Guitar Pro tab of that song) pairs into labelled test data
(roadmap Phase 0.3). Use only recordings and tabs you own; don't download tabs from tab sites.
The tab (GP3-5, GPX or GP7/8, read by `gp_tabs.py`) is aligned to the recording with
synctoolbox's MrMsDTW on chroma + DLNCO onset features computed from the tab's notes (all
pitched tracks), so no rendering is needed. Each guitar track's notes are written in recording
time, plus a copy of the tab and a `.gvtab.json` sidecar that makes the Tabs page play the
recording in sync with the cursor, for hand-checking.

```bash
# Once: a venv with synctoolbox (CPU only).
uv venv -p 3.12 ~/.local/share/guitar_virtual_amp/tools/align-venv
VIRTUAL_ENV=~/.local/share/guitar_virtual_amp/tools/align-venv uv pip install synctoolbox pyguitarpro mir_eval mido soundfile
AL=~/.local/share/guitar_virtual_amp/tools/align-venv/bin/python
# 1. List the pairs: audio <TAB> tab [<TAB> id [<TAB> track indices]], paths relative to the file.
printf 'audio/Song A.flac\ttabs/Song A.gp5\tsong_a\n' >> ~/gva_real/pairs.tsv
# 2. Align (10-20 s per 3-minute song on one core, 0.6 GB RAM; aligned pairs are skipped).
$AL align_tabs.py align --pairs ~/gva_real/pairs.tsv --out ~/gva_real
# 3. Hand-check: open ~/gva_real/check/<id>.* in the app's Tabs page (it plays the recording
#    from the sidecar), listen around the bars the checklist names as the worst fit, then set
#    the first word of the line in ~/gva_real/checklist.txt: todo, checked or rejected.
#    Optional: load the copy and sidecar through the app's own code and compare its notes.
cargo run --release --example tab_sync_check -- ~/gva_real/check/song_a.gp5 0 ~/gva_real/pairs/song_a/notes/1_<track>.txt
# 4. Manifests: eval_real.tsv (checked pairs, union of the guitar tracks, group `real`),
#    eval_real_tracks.tsv (checked, one row per guitar track, `real/guitar<k>`),
#    train_real_unchecked.tsv (accepted by the aligner, not checked yet).
$AL align_tabs.py manifest --out ~/gva_real
# 5. Score.
GVA_EVAL_MANIFEST=~/gva_real/eval_real.tsv GVA_EVAL_DEMUCS=1 \
    cargo test --release --workspace --exclude guitar_android eval_manifest -- --ignored --nocapture
$PY bench_notes.py run --data ~/gva_real --manifests eval_real.tsv --out ~/gva_real/bench --engine crnn:gaps=guitar-gaps.pth --limit 0
$PY bench_notes.py score --data ~/gva_real --manifests eval_real.tsv --out ~/gva_real/bench --limit 0
$AL align_tabs.py score-tab --out ~/gva_real --bench ~/gva_real/bench --notes-engines crnn-gaps \
    --fingering-bin target/release/examples/fingering   # string accuracy and Tab F1 (bench_tab.py's metrics)
```

Notes files have `onset offset pitch velocity technique string fret` per line (the first five
are what `eval_manifest` and `bench_notes.py` read; techniques come from the tab, as for GOAT;
dead notes are left out). The aligner rejects a pair when `cost_ratio` (chroma distance along
the warping path over the distance of random frame pairs) is above 0.70, when `stall` (share of
the song where the local tempo along the path is off by more than 2x: a section one side has
and the other doesn't) is above 0.10, or when the best transposition is more than 2 semitones;
a transposition of 1-2 semitones (a tab notated in E standard for a song tuned down) is applied
to the labels and noted in the checklist. Marking a rejected pair `checked` overrides the
verdict. Re-aligning a pair (`--force`) resets its `checked` mark. Tabs with repeats get a copy
with every pass written out, because the sidecar sync is per bar.

Validation (`validate_tab_align.py`, CPU, on EGSet12 and GOAT, whose recordings follow their
tabs; the tab is also distorted the way a commercial recording differs from it: per-bar tempo
drift and 2-6 s of intro, one semitone of detuning, the recording over MUSDB18 backing from
`make_mixes.py`, 15-25% of the bars cut, another song's tab):

```bash
$AL validate_tab_align.py --egset12 ~/.cache/gva_test/egset12 --goat ~/.cache/gva_test/goat/GOAT \
    --goat-train 12 --mixes mixes/eval_mix.tsv --out val --jobs 12   # summary in val/summary.md
```

`--mixes` is a `make_mixes.py` manifest made from rows `<recording> <TAB> <label file (.jams
or _fine_aligned.mid)> <TAB> <group>`, so each mix can be traced back to its item.
Result with the defaults: aligned-tab notes vs labels, note F1 at 50 ms 0.92 on EGSet12 (median
onset error 12 ms) and 0.80 on the GOAT test split (18 ms), 0.84 / 0.73 with tempo drift and
intro; 121 of 128 correct pairs accepted, 0 of 168 wrong-tab pairs and 0 of 28 cut tabs.
synctoolbox's default onset-peak shift (-25 ms) made every note ~20 ms early, hence
`ONSET_OFFSET_MS = 0`.

## Learned fingering (string per note, given the notes)

`fingering/` trains a small tagger that predicts each note's string from the notes around it
(roadmap T2, encoder-only first step) and benchmarks it against the app's Viterbi with
`bench_tab.py`'s readers and scoring. Per note, the model sees the fret the pitch would have on
each string (or "can't be played there"), so it doesn't depend on the tuning, plus onset gaps,
duration, chord size and order, and intervals to the neighbours. A 4-layer pre-norm Transformer
(d = 128, a depthwise convolution for order, no position tables; 0.57 M parameters) gives
string logits with unplayable strings at -1e4. A decoder then puts each chord (onsets within
50 ms) on distinct strings, with a stretch cost and a Viterbi over hand position between chords
(`common.decode`: span 1.0, move 0.5).

```bash
PY=python  # needs torch, onnx, onnxruntime, mido, mir_eval, pyguitarpro (GOAT tabs)
# 1. Notes with strings, train/val/test (GuitarSet 00-02 / 03-05, EGDB id % 5, EGSet12 test,
#    Guitar-TECHS P1/P2 chords/scales/single notes, GOAT official split).
$PY fingering/data.py --root ~/.cache/gva_test --out fingering_data/data.json
# 2. Cross-player check for choices: GuitarSet player 02 is val, the rest trains.
$PY fingering/train.py --data fingering_data/data.json --out runs/cv --val-player 02 --transpose 0
# 3. Final models: fixed steps and decoding costs from step 2. "ship" has no GOAT; "full"
#    adds GOAT train at 0.1 sampling weight (non-commercial, not shippable).
for s in 1 2 3; do
  $PY fingering/train.py --data fingering_data/data.json --out runs/ship_s$s --variant ship --seed $s \
      --transpose 0 --steps 3000 --fixed-steps --decode 1.0,0.5,0.0
  $PY fingering/train.py --data fingering_data/data.json --out runs/full_s$s --variant full --seed $s \
      --goat-weight 0.1 --transpose 0 --steps 3000 --fixed-steps --decode 1.0,0.5,0.0
done   # ~4 min each on the GB10; train.py also exports model.onnx (export.py) and checks it
# 4. Benchmark (laptop: the Viterbi binary is x86). --limit 12 = bench_tab.py's item lists.
cargo build --release --example fingering
$PY fingering/bench.py --data ~/.cache/gva_test/prepared --guitarset ~/.cache/gva_test/guitarset \
    --egdb ~/.cache/gva_test/egdb --egset12 ~/.cache/gva_test/egset12 --bench ~/.cache/gva_test/bench \
    --fingering-bin target/release/examples/fingering --fing-data fingering_data/data.json \
    --model ship=runs/ship_s1/model.onnx,runs/ship_s2/model.onnx,runs/ship_s3/model.onnx --limit 0
```

- Don't transpose the training passages (moving every fret by k is label-preserving, but
  `--transpose 7` dropped GuitarSet solo accuracy on new players from 0.62 to 0.43): where a
  player puts a phrase depends on the key, and the model needs that.
- Simulated transcription noise (dropped, extra and jittered notes) is on by default
  (`--no-noise` turns it off). With transposition on, removing it cut cross-player solo
  accuracy from 0.65 to 0.46. Without transposition, one no-noise seed was mixed on the tests:
  better on GOAT and GuitarSet comp, worse on the EGDB tones and the separated stems.
- Choose settings with `--val-player`: a random 10% val split of GuitarSet has the same players
  as training, and the model reaches 0.95+ on them (memorized), against ~0.62 on new players.
- `export.py` writes `<model>.onnx.json`: op list (Gather, MatMul, Conv, Softmax, ReduceMean,
  Pow, Sqrt, Equal, Not, Cast, Reshape, Transpose, Split, ...; opset 17, LayerNorm written out),
  max logit difference against PyTorch, and the decoding costs. Inputs `frets` [1, T, 6] int64
  and `cont` [1, T, 16] float32 with T symbolic. tract-onnx 0.23.10 runs it (max |diff|
  < 1e-5 against onnxruntime). Recordings longer than 128 notes run in windows of 128 with
  hop 64 (`common.windowed`).
