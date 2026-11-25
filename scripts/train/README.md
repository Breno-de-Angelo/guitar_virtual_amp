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
