# Automatic tablature: evaluation and model roadmap

Research plan for the `transcribe/` pipeline. The goal is the best tab we can get from a
commercial recording, using any model or dataset whose license allows non-commercial use.
Status and measured numbers of the current pipeline are in `ROADMAP.md` ("Automatic
tablature"); this file covers what to measure next, where each candidate method is expected to
fail, and which models are worth training.

Ground rules:
- **Measure before switching.** Every change goes through the same benchmark matrix (Phase 0) and
  is kept only if it wins on the sets it targets without losing on the others.
- **Downstream metrics decide.** A separator is judged by the note F1 of the tab made from its
  stem, not by SDR alone; a note model by the final tab, not only by its raw F1.
- **Hardware:** training and data generation on several DGX Sparks (see "Compute plan");
  the RTX 4070 Laptop (8 GB, 14 GB RAM) stays the development and **inference target**: any model
  the app uses must run there (or on CPU), whatever size it was trained at.
- **Runtime:** Python sidecars are acceptable (as Demucs is today). Porting to tract/ONNX comes
  after a model has won.
- **"Known"** failure modes below are measured here or reported by the authors; **"expected"**
  ones are hypotheses that the benchmark has to confirm.

---

## Phase 0: benchmark that matches the literature

The current numbers (`ROADMAP.md`) are note F1 at 80 ms on the final quantized tab. Papers report
other metrics, and there is no real band-recording test set yet. Both block every later decision.

### 0.1 Metrics (add to `transcribe/eval.rs`)

| Metric | Definition | Compares with |
|---|---|---|
| Note F50 | mir_eval onset F1, 50 ms, pitch, on raw notes before quantization | MT3, YourMT3+, MuScriptor, GAPS, TART stage 1 |
| Note F50 + offset | mir_eval with offset tolerance max(50 ms, 20% of duration) | MuScriptor, Kong et al. |
| Tab F1 | Note F50 match that also has the same string and fret | TART (71.8% string-fret, 54.08% end-to-end), Noise2Fret |
| String accuracy | Share of matched notes on the reference string (exists: `eval_fingering`) | MIDI-to-Tab, Fretting-Transformer |
| Final-tab F1 | Current metric (80 ms, after quantization) | Our history |
| Octave errors | Share of reference notes found only an octave or two away (exists) | Distortion diagnosis |
| Technique P/R/F1 | Per class: bend, release, vibrato, slide, hammer/pull, palm mute, harmonic, dead note | TART stage 2, IDMT papers |
| Track count | Right number of guitar tracks (exists: `eval_guitar_pairs`) | None published |
| Cost | Wall time and peak RAM for a 4-minute song, CPU and GPU | Feasibility |

### 0.2 Test-set matrix

Splits must stay fixed: any set used for training (Phase 2) keeps its test part out.

| Set | Audio | Labels | On disk | Tests |
|---|---|---|---|---|
| GuitarSet players 03-05 | Solo acoustic, mic | Notes + strings | Yes | Clean baseline, fingering |
| GuitarSet x presets (`GVA_EVAL_TONE`) | Same, through the app's chains | Same | Yes | Tone robustness |
| EGDB test (`eval_egdb`) | Electric DI, Marshall, Mesa, Plexi, 5 real recordings | Notes + strings | Yes (48 per tone) | Amp tones, fingering on electric |
| Guitar-TECHS P3 + techniques | Electric DI and miked amp | Notes (hexaphonic MIDI), techniques | Yes (12 + 12) | Real electric, techniques |
| IDMT-SMT-Guitar | DI, isolated notes and licks | Notes + expression style | Yes (657, 86 tech) | Techniques |
| GOAT test | DI + 5 amp renders | Guitar Pro tabs with techniques | Yes (7 x 6 tones) | Tab F1, techniques. Small: report per item |
| Mixes (`make_mixes.py`) | Labelled guitar over MUSDB drums/bass/vocals | Notes | Yes (77) | Separation, full mix |
| GP SoundFont renders (`eval_transcription`) | User's Guitar Pro files, synthesized | Tabs | Yes | Whole pipeline, multi-track |
| **Real songs (new, 0.3)** | Commercial recordings | User's GP tabs aligned to the audio | No | The actual use case |

### 0.3 New: real-song benchmark by tab-to-audio alignment

MuScriptor built its 11k-hour real dataset by aligning symbolic scores to recordings: bar-line
interpolation where bar positions are known, otherwise DTW on chroma + onset features with the
Sync Toolbox (Müller et al. 2021), and rejection of pairs with a high warping cost. The same
works here:
1. Pick 30-50 songs from the user's tab library that have an official or high-quality GP tab.
2. Render the tab (synth already exists), align render to recording with `synctoolbox`, reject
   bad warps.
3. Hand-check 20-30 of them in the tab player (`SyncMap` already exists) and keep only those.
4. Score with the Phase 0.1 metrics. Tab errors (wrong positions, simplified parts) limit the
   ceiling: report it as "agreement with a human tab", not ground truth.

The aligned set doubles as **training data** for Phase 2 (keep the hand-checked songs for test).

**Exit criterion for Phase 0:** one command produces the full matrix (sets x metrics) for a
pipeline configuration, and the current pipeline's row is recorded as the baseline.

---

## Phase 1: evaluate existing methods, stage by stage

Each stage swaps one component and keeps the rest. Candidates are listed by expected payoff.

### 1.1 Source separation

| Candidate | Notes | Known / expected failures |
|---|---|---|
| Demucs `htdemucs_6s` (current) | MIT | Known: weak guitar stem (MSST lists no guitar SDR for it). Expected: guitar leaking into `other`/`piano`, synth pads and organ into `guitar`, transient smearing that creates false onsets |
| [BS-RoFormer SW 6-stem](https://huggingface.co/FractalSurfer/BS-RoFormer-SW-6stem) | Community checkpoint run in MSST; original author account deleted, license unclear | Expected: better isolation, slower (transformer on full STFT), same one-stem-for-all-guitars limit |
| [Banquet](https://arxiv.org/abs/2406.18747) (query-based) | Beat `htdemucs_6s` on guitar on MoisesDB with 24.9M parameters; can query narrow classes (clean acoustic guitar) | Expected: query by class, not by part: two electric guitars still come out together |
| [MVSep guitar](https://mvsep.com/algorithms/17) (MDX23C/RoFormer ensemble) | Web service; check whether weights are downloadable | Online only, ToS for uploads |
| No separation, MuScriptor on the mix (1.2) | Tests whether separation is needed at all | See 1.2 |

How to measure: final-tab F1 on the Mixes set and the real-song set, per separator; SDR on
MoisesDB guitar stems only as a diagnostic. Also check that the pan split (`stereo.rs`) still
works on each separator's stereo output (`eval_guitar_pairs`).

### 1.2 Note transcription (guitar)

| Candidate | Notes | Known / expected failures |
|---|---|---|
| GAPS CRNN (current) | Kong et al. architecture, classical guitar weights | Known: 0.24 on "Metal High Gain", octave errors mostly an octave low, 0.02 on a low 8-string riff; trained on solo guitar, so a stem with bleed confuses it |
| Fine-tuned electric CRNN (`runs/electric_v1`, `electric_gs_v2`) | In progress; 0.807 validation onset F1 on EGDB test tabs (from 0.717) | Expected: improves amp tones; still sees only clean, isolated guitar during training, so separation artifacts remain unseen (see T1) |
| Basic Pitch (current fallback) | Small, generic | Known: reports string harmonics as notes; 0.32 on high gain |
| [MuScriptor](https://github.com/muscriptor/muscriptor) small / medium / large | Decoder-only transformer, 100M / 300M / 1.3B, trained on 11k hours of real aligned music. "Distorted E-Guitar" is its 2nd most frequent class (in ~47% of tracks), "Clean E-Guitar" and "Acoustic Guitar" are separate classes; instrument conditioning. CC-BY-NC weights | Known: cannot represent two notes of the same pitch on the same instrument (onset F1 drops from 60.4 to 51.8 on its test set when such notes are kept); no velocity; offsets weaker than onsets; 5 s segments. Expected: all guitars in one class merged into one part; clean vs distorted class confusion splitting one guitar into two; slow autoregressive decoding on CPU |
| [YourMT3+](https://github.com/mimbres/YourMT3) (YPTF.MoE) | Multi-instrument, open | Known (MuScriptor paper): poor on real-world mixes; MuScriptor beats it by a wide margin |
| MuScriptor on the separated stem vs on the mix | Its instrument conditioning may be enough without Demucs | Expected: stem input removes confusion with keys/synths, but MuScriptor was trained on mixes, so stems are out of its distribution |
| TIT ([EGDB-PG](https://arxiv.org/abs/2504.07406), 2025) | Tone-embedding transcriber trained on many amp/cab configurations | Check whether weights are released |
| [EG-VAE](https://arxiv.org/pdf/2608.05513) tone removal as a preprocessor | Turns amped guitar back into something like DI before transcription | Check weights. Expected: helps the CRNN on distortion, may invent or smear notes on dense chords |

How to measure: Note F50 and octave errors on every set of the matrix; MuScriptor once with
conditioning `{distorted_electric_guitar}`, once with the song's full instrument set, once without.

### 1.3 Beats, bars, meter

Current: Beat This! `small0` + bar-pointer Viterbi. Tempo right 59/60, bar lines 57/60 on
GuitarSet. Low priority, but cheap to check.

| Candidate | Known / expected failures |
|---|---|
| Beat This! `final0` (bigger checkpoint of the same model) | Expected: small gain over `small0` |
| [AlignBeat](https://arxiv.org/abs/2510.14391) | Reports +3.6 points downbeat CMLt over Beat This! |
| [Explore This!](https://arxiv.org/abs/2610.14817) (learned tatum grid, 2026) | New; checks whether a tatum grid helps our 16th/triplet quantizer |
| All neural trackers | Known ([SMC blind spot](https://arxiv.org/abs/2605.12287)): confident-but-wrong on slow, rubato and expressive music, double-tempo on slow songs. Ours: compound meters written as 2/4 or 6/4 with triplets instead of 6/8 |

How to measure: bar-line alignment and Final-tab F1 on the GP renders and the real-song set
(GuitarSet has almost no meter variety).

### 1.4 Techniques

Current: rules on Basic Pitch's pitch contour, single-note lines only, checked on synthetic tones
only. Labelled data is already on disk (IDMT, Guitar-TECHS techniques, GOAT tabs), so this is the
cheapest stage to measure.

| Candidate | Known / expected failures |
|---|---|
| Current rules | Known: no palm mute, harmonics, dead notes, tapping; chords skipped. Expected: Basic Pitch's contour gets noisy under distortion, so bends/vibrato are missed exactly where they are most common; hammer-on rule (no pick attack) breaks with compression and high gain |
| TART stage 2 (MLP technique classifier) | Check whether code/weights are released; trained on clean data |
| Per-note classifier we train (T3) | See Phase 2 |

How to measure: technique P/R/F1 per class on IDMT tech, Guitar-TECHS techniques and GOAT test,
clean and through presets.

### 1.5 Fingering (string and fret)

Current: Viterbi over hand positions with a GuitarSet string prior. 75.4% string accuracy on
GuitarSet 03-05, 65.4% on 10 human GP tabs.

| Candidate | Known / expected failures |
|---|---|
| Current Viterbi | Known: weights fitted on few tabs; no long-range context (riff repeated in another position, idiomatic shapes, open-string pedal tones) |
| [MIDI-to-Tab](https://arxiv.org/abs/2408.05024) (BART, DadaGP pre-training) | Check weights. Expected: copies tab-site conventions (DadaGP is user tabs); blind to audio |
| [Fretting-Transformer](https://arxiv.org/abs/2506.14223) (T5) / [open-fret](https://github.com/Sidmaz666/open-fret) reimplementation | Check weights; same expectations |
| [Note-event tokenization + pitch-valid decoding](https://arxiv.org/html/2607.26440) (2026) | Beats Fretting-Transformer, much better on small data; constrained decoding removes impossible string/pitch pairs |
| TART stage 3 (audio-conditioned T5) | Uses timbre to tell strings apart (the same note sounds different on the B string and the G string); check weights |

How to measure: string accuracy and Tab F1 with true notes (`eval_fingering`) and with
transcribed notes, on GuitarSet 03-05, EGDB test, GOAT test, user tabs, real-song set.

### 1.6 Tab assembly

Not model-dependent, but visible to the user. Evaluate on the GP renders and real-song set:
tuning detection (drop D, half step down, 7/8 strings), capo, quantization (16th triplets, 32nds,
6/8), repeated sections written consistently.

### 1.7 Bass

Current: Basic Pitch. Compare with MuScriptor ("E-Bass" is its 3rd most frequent class) on the
real-song set. Bass fingering uses the same Viterbi.

**Exit criterion for Phase 1:** one winning configuration per stage, with the matrix row recorded.
That configuration is the baseline for Phase 2.

---

## Phase 2: models to train

Ordered by payoff per effort. Each lists data, compute, and the specific failure it targets.
Compute assumes the DGX Sparks ("Compute plan"); times are estimates until the throughput probe
there is done.

### T1. Electric guitar CRNN, separation-aware (extends the run in progress)

Targets: distortion octave errors, notes lost to separation artifacts.
- Keep the current recipe (EGDB, Guitar-TECHS, GOAT, GuitarSet; many tones per performance).
- **Train on what inference sees:** mix labelled guitar over backing (`make_mixes.py`, MUSDB and
  MoisesDB), run the chosen separator from 1.1, and train on the separated stem with the original
  labels. The model then learns the separator's bleed and artifacts.
- More tones: the app's presets (`render_tones`), NAM captures the user owns. TONE3000 terms
  forbid bulk download, so no scraping their catalog for training.
- Optional pseudo-labels: MuScriptor-large guitar notes on real songs, kept only where they agree
  with the aligned GP tab (0.3).
- Compute: hours per run (laptop runs: ~1-2.5 s/step, 5-8k steps). On the Sparks: one
  configuration per node (tone sets, with/without separated stems, learning rates) instead of one
  run at a time; larger batches also allow unfreezing BatchNorm, which the laptop recipe keeps
  frozen because of batch size 4. Stays a CRNN of the same size, so tract inference is unchanged.
- Measure: Note F50 on EGDB/Guitar-TECHS/GOAT tests per tone, Mixes set through the separator.

### T2. Fingering transformer

Targets: the 65-75% string accuracy ceiling of the Viterbi.
- Model: T5-small-sized encoder-decoder (~60M), note-event tokens, pitch-valid constrained
  decoding (2607.26440), tuning as a prefix token.
- Data: DadaGP (26k GP songs, research access on request), then fine-tune on GuitarSet 00-02,
  EGDB train, GOAT train. Train augmented with transcription noise (dropped, extra and shifted
  notes from our own CRNN outputs) so it tolerates real input.
- Step 2: condition on audio. Add the CRNN's frame embeddings at each note onset to the encoder
  (as TART's audio-conditioned T5); GuitarSet, EGDB and GOAT give string labels with audio.
- Compute: one Spark, well under a day for DadaGP pre-training at this size; run seeds and
  tokenization variants on separate nodes. ~60M parameters still runs on CPU in the app.
- Measure: 1.5 metrics; must beat the Viterbi on held-out GuitarSet 03-05 and real-song set.

### T3. Per-note technique classifier

Targets: missing palm mute, harmonics, dead notes; rules failing under distortion.
- Input: CQT patch and Basic Pitch contour around each detected note (or the CRNN's embeddings).
- Output: multi-label technique set per note, plus bend height.
- Data: GOAT (techniques from the tabs), IDMT-SMT-Guitar (expression styles), Guitar-TECHS
  techniques, all through tone augmentation; SynthTab (DadaGP synthesized with plugins, includes
  a subset of techniques) for volume.
- Alternative: add technique heads to the T1 CRNN (multi-task), one model instead of two.
- Compute: small CNN, an hour-scale run on one node.
- Measure: technique P/R/F1; keep the rules for any class where the model is worse.

### T4. Guitar tab transformer on full mixes (MuScriptor fine-tune)

The proposal to go beyond the published state of the art: one model that reads the mix and
writes guitar notes **with string and fret**, conditioned on guitar type.
- Why it could win: MuScriptor already handles real mixes and distorted guitar; tab tokens add
  fingering from timbre (the TART stage 3 idea) in the same pass, and adding the string to each
  note token fixes MuScriptor's same-pitch limitation (a unison on two strings becomes two
  different tokens).
- Model: full fine-tunes of all three sizes fit on one Spark. 1.3B parameters with bf16 weights,
  fp32 master copy and AdamW take ~21 GB before activations, against 128 GB of unified memory.
  Plan: small (100M) first to validate the tokens and data, then large (1.3B) with DDP across
  the Sparks for throughput.
- Deployment: large runs on the laptop for inference (~2.6 GB of bf16 weights in 8 GB) but
  slowly; if it is too slow, **distill** it into small or medium on the Sparks (large labels
  SynthTab-scale audio and real songs, small learns from those labels).
- Tokens: keep MT3 note tokens, add `string`, `fret` (and later technique) tokens per note.
- Data:
  - Isolated guitar with strings: GuitarSet, EGDB, GOAT, Guitar-TECHS.
  - Synthetic at scale: SynthTab, or DadaGP tabs rendered through our synth + amp chains;
    [GuitarFlow](https://arxiv.org/pdf/2510.21872) (tab to realistic electric guitar audio) is a
    candidate renderer if weights exist.
  - Mixed over MUSDB/MoisesDB backing, at varied levels and pans.
  - Real songs aligned to GP tabs (0.3), hand-checked ones kept for test.
- Risks: slow autoregressive inference (CPU); 5 s segments make bars inconsistent across
  boundaries (keep our beat/arrange stages for layout); one class for several guitars (keep the
  pan split, or condition on side).
- Gate: only start after T1/T2 baselines exist, so the gain is measurable.
- Measure: Tab F1 and Note F50 on the Mixes and real-song sets, against the best Phase 1
  pipeline.

### T5. Separating two guitars in the same spot of the mix (research, lowest priority)

Targets: rhythm and lead panned together stay one track today.
- Data: pairs of different labelled performances (EGDB, Guitar-TECHS, GOAT DI) rendered through
  different tones and summed at the same pan, plus MoisesDB guitar sub-stems.
- Model: Banquet-style query separator queried by "rhythm"/"lead" or by tone embedding, or a
  BS-/Mel-RoFormer fine-tuned in MSST from the 6-stem checkpoint. RoFormer training is
  memory-hungry, which the Sparks handle; inference on the laptop needs checking.
- Expected to fail when both guitars share tone and register (doubled parts), which is also when
  merging them is acceptable.

---

## Compute plan (DGX Spark)

What a DGX Spark is good and bad at, for this work:
- **Memory:** 128 GB unified CPU/GPU memory per node, 16x the laptop's GPU. This is the main
  gain: full fine-tunes up to MuScriptor-large, big batches, whole datasets cached in RAM.
- **Bandwidth:** 273 GB/s, about the same as the laptop GPU. Memory-bound work (autoregressive
  decoding, small-batch training) gets little faster per node; compute-bound work (convolutions,
  large-batch transformer training) gains more. The "1 PFLOP" figure is sparse FP4, not the BF16
  used for training.
- **CPU:** ARM64 (Grace). Use NVIDIA's PyTorch containers (NGC) rather than pip wheels; check
  that Demucs, MSST, MuScriptor and `synctoolbox` install there. `render_tones` and the Rust eval
  build for aarch64 as is.
- **Several nodes:** NVIDIA's NCCL playbooks cover 2-4 node clusters over the 200 Gb/s ConnectX-7
  links. Use DDP only for T4-large; everything else runs one independent job per node.

Job layout, assuming N nodes:

| Work | Parallelism | Notes |
|---|---|---|
| Phase 0/1 evaluation matrix | One configuration per node | MuScriptor small/medium/large, separators, CRNN checkpoints all evaluated at once |
| Data generation | Split the song list across nodes | Tone renders of DI recordings and DadaGP tabs, separated stems of synthetic mixes, MuScriptor-large pseudo-labels, tab-to-audio alignment (0.3) |
| T1, T2, T3 | One run per node | Sweeps instead of single runs |
| T4 small | One node | Validate tokens and data |
| T4 large | DDP over all nodes | Only after T4 small beats the Phase 1 baseline |

**Throughput probe** (`scripts/train/probe_throughput.py`, CRNN fine-tune recipe, 4 s crops,
real data loader, 2026-11-24, first Spark `spark-ef6b`):

| Setup | Precision | Clips/s | Peak GPU memory | vs laptop |
|---|---|---|---|---|
| Laptop 4070, batch 4 x accum 2 (`electric_gs_v2` log, 1.0 s/step) | TF32 | ~8 | <8 GB | 1x |
| Spark, batch 4 x accum 2 (same recipe) | TF32 | 12.5 | 4.3 GB | 1.6x |
| Spark, batch 32 | TF32 | 16.9 | 28 GB | 2.1x |
| Spark, batch 32 | bf16 autocast | 26.7 | 15 GB | 3.3x |

- Per node the Spark is 1.6-3.3x the laptop, not more: throughput flattens above batch 8 (TF32
  15.2 clips/s at batch 8, 16.7 at batch 64 with 56 GB). The gain is memory and node count.
- Data loading on the ARM cores is not a bottleneck (synthetic and real batches give the same
  speed).
- bf16 is untested for accuracy: the recipe keeps fp32 because fp16 overflowed in the power
  spectrum. bf16 has fp32's range, but validation F1 must match a TF32 run before adopting it.
- A run like `electric_gs_v2` (8000 steps x 8 clips) takes ~1 h in TF32 or ~40 min in bf16 on
  one Spark, against ~2.2 h on the laptop.
- Not yet measured: MuScriptor training and inference (needs its package and weights on the
  Spark).

Spark setup notes: DNS was broken by Tailscale overwriting `/etc/resolv.conf` with MagicDNS
(100.100.100.100) while the tailnet has no global nameservers. Fixed on `spark-ef6b` with
`sudo tailscale set --accept-dns=false` and `/etc/resolv.conf` linked back to
`../run/systemd/resolve/stub-resolv.conf` (old file kept as `/etc/resolv.conf.tailscale-old`);
the other Sparks likely need the same. Internet downloads measured ~9 MB/s, about the same as
copying from the laptop. The training venv is `~/gva/venv`, built on miniforge's base Python 3.13, which already has
PyTorch 2.13 + CUDA 13.0 for the GB10 (compute capability 12.1). Data is under the same absolute
paths as on the laptop (`~/.cache/gva_test`), so `train.jsonl` and the 16 kHz caches work
unchanged; the raw audio is not copied (only `prepared/`, `prepared_gs/`, `guitar-gaps.pth`).

Data: `~/.cache/gva_test` is ~60 GB today (plus GOAT, DadaGP and SynthTab later). Put it on shared
storage or copy it to each node once; the 16 kHz caches (`cache16k`) avoid decoding audio on the
ARM cores at every epoch.

## Results so far

### Note benchmark, CRNN baselines (2026-11-24)

`scripts/train/bench_notes.py`, mir_eval onset F1 at 50 ms (F50), up to 12 items per group spread
evenly, mean per item; raw model notes, no quantization; band mixes go to the model **without**
separation. Same note-reading rules as `guitar_crnn.rs`. `electric_v1` = GAPS fine-tuned on EGDB
train + Guitar-TECHS P1/P2 with tone renders (no GOAT, no GuitarSet).

| Group | n | GAPS | electric_v1 |
|---|---|---|---|
| EGDB test DI / Marshall / Mesa / Plexi | 12 each | 0.80 / 0.75 / 0.77 / 0.66 | 0.86 / 0.83 / 0.84 / 0.80 |
| EGDB real recordings | 5 | 0.89 | 0.88 |
| GOAT test DI | 7 | 0.82 | 0.84 |
| GOAT test, 5 amp renders (mean) | 7 each | 0.56 | 0.71 |
| Guitar-TECHS P3, DI / mic'd amp | 12 each | 0.82 / 0.81 | 0.78 / 0.78 |
| GuitarSet 03-05 comp / solo | 12 each | 0.87 / 0.91 | 0.84 / 0.91 |
| Mixes, no separation (EGDB Marshall / real / TECHS DI / TECHS amp) | 12 / 5 / 12 / 12 | 0.52 / 0.66 / 0.49 / 0.39 | 0.53 / 0.64 / 0.45 / 0.37 |

- The fine-tune wins on amp tones: +0.15 F50 on GOAT amp renders, which it never saw, and
  octave-low errors halve there (8% to 5% of notes). Offsets improve everywhere
  (F50+offset on GOAT amps 0.33 to 0.52).
- It loses a little on clean recordings outside its training data (GuitarSet comp -0.03,
  Guitar-TECHS P3 -0.03). `electric_gs_v2` adds GuitarSet 00-02 to address the first.
- On full mixes without separation both CRNNs sit at 0.37-0.66, so separation (or MuScriptor)
  is where the large gains are.
- Speed on the Spark GPU: ~29x real time for either CRNN.

### electric_gs_v2 (adds GuitarSet players 00-02 to the training data)

Two runs of the same recipe: the laptop run stopped at step ~4900 (best checkpoint step 4000,
validation 0.8052); the Spark TF32 run (`runs/spark_tf32`, copied back to the laptop) ran all 8000
steps, best checkpoint step 5000, validation 0.8123. Benchmark as above (GuitarSet 03-05 is held
out from both):

| Set | GAPS | electric_v1 | v2 laptop (s4000) | v2 Spark (s5000) |
|---|---|---|---|---|
| EGDB test DI | 0.80 | 0.86 | 0.85 | 0.86 |
| EGDB test amps (Marshall, Mesa, Plexi) | 0.72 | 0.82 | 0.83 | 0.83 |
| EGDB real | 0.89 | 0.88 | 0.89 | 0.90 |
| GOAT DI | 0.82 | 0.84 | 0.84 | 0.84 |
| GOAT amps (5) | 0.56 | 0.71 | 0.71 | 0.71 |
| Guitar-TECHS P3 (DI, amp) | **0.81** | 0.78 | 0.77 | 0.78 |
| GuitarSet comp | 0.87 | 0.84 | 0.88 | **0.89** |
| GuitarSet solo | 0.91 | 0.91 | 0.92 | 0.92 |
| Mixes, no separation (4) | 0.51 | 0.50 | 0.51 | 0.50 |
| F50+offset, GuitarSet comp / solo | 0.28 / 0.56 | 0.47 / 0.60 | 0.42 / 0.59 | **0.59 / 0.76** |

- v2 fixes v1's GuitarSet regression (comp 0.84 to 0.89, above GAPS) and keeps the amp-tone gains.
- The full 8000-step run has much better offsets (GuitarSet solo 0.76 vs 0.59-0.60), which matter
  for note lengths in the tab.
- Still behind GAPS on Guitar-TECHS P3 (-0.03), the only set where the fine-tunes lose.
- EGDB scores of the fine-tunes are slightly optimistic: EGDB DI/Plexi/real is the validation set
  used to pick the checkpoint.
- Candidate for the app's default model: `runs/spark_tf32/model.onnx`. Confirm with the Rust
  final-tab benchmarks (`eval_guitarset`, `GVA_EVAL_TONE`) before switching.

### bf16 vs TF32 training

bf16 autocast (Spark copy of `finetune_crnn.py` only) crashed at step ~1100: predictions outside
[0, 1] (NaN) reached the BCE loss, then a cuDNN error. Up to then it tracked TF32 (0.7793 vs 0.7827
validation at step 1000). Not worth pursuing: the full TF32 run takes ~1 h alone on a Spark
(this one took ~1 h 50 min sharing the GPU with the bf16 run). Use TF32.

### MuScriptor

Blocked: the Hugging Face repos `MuScriptor/muscriptor-{small,medium,large}` are gated, and the
account `Breno-de-Angelo` (logged in on the Spark) is not yet approved. The package is installed
in `~/gva/venv` on the Spark; once access is granted, run
`bench_notes.py run --engine muscriptor:{small,medium,large}:{free,known}` and `score`.

## Order of work

Two tracks can run at once now: evaluation and data generation on the Sparks, benchmark code and
the real-song set on the laptop.

1. Phase 0.1 metrics and the matrix runner (unblocks everything). In parallel: Spark setup and
   throughput probe.
2. Phase 1.2 MuScriptor (all sizes) and 1.4 techniques on the matrix, on the Sparks.
3. Phase 0.3 real-song set (slowest to build; start early).
4. Data generation on the Sparks: tone renders, separated mixes, DadaGP renders, pseudo-labels.
5. T1 sweep with separation-aware data; Phase 1.1 separators and 1.5 fingering candidates.
6. T2 and T3 in parallel.
7. T4 small, then T4 large if small wins. With the Sparks this no longer waits on a rented GPU,
   so it moves up from "only if needed" to the main bet for full mixes.
8. T5 only if multi-guitar songs remain the main error source.

## Licenses (non-commercial use)

| Resource | License / terms | Note |
|---|---|---|
| MuScriptor weights | CC-BY-NC | Attribution required; fine-tuned weights inherit NC |
| GOAT | Non-commercial | Training allowed for this app |
| MUSDB18 | Non-commercial research | Backing for mixes |
| MoisesDB | Check terms before use | Guitar sub-stems |
| DadaGP | Research access on request | Fingering pre-training |
| IDMT-SMT-Guitar | CC BY-NC-ND | Test only (no derivatives) |
| GuitarSet, Guitar-TECHS | CC BY 4.0 | |
| GAPS weights, Beat This!, Demucs | MIT | |
| BS-RoFormer SW checkpoint | Unclear (mirror of a deleted account) | Use locally only |
| YouTube audio | YouTube ToS forbid downloading | Unchanged from `ROADMAP.md`: personal use only, not in a store build |
| TONE3000 captures | API terms: no bulk download or caching of the catalog | Use only captures the user downloaded for playing |

## Sources

- DGX Spark: [NVIDIA, performance overview](https://developer.nvidia.com/blog/how-nvidia-dgx-sparks-performance-enables-intensive-ai-tasks/)
  (128 GB unified memory, 273 GB/s, 1 PFLOP FP4);
  [NCCL multi-node playbook](https://github.com/NVIDIA/dgx-spark-playbooks/blob/main/nvidia/nccl/README.md).

- MuScriptor: Rouard et al., 2026, [arXiv 2607.08168](https://arxiv.org/abs/2607.08168).
- YourMT3+: Chang et al., MLSP 2024, [arXiv 2407.04822](https://arxiv.org/abs/2407.04822).
- MT3: Gardner et al., ICLR 2022, [arXiv 2111.03017](https://arxiv.org/abs/2111.03017).
- TART: Gupta et al., ISMIR 2026, [arXiv 2609.11904](https://arxiv.org/abs/2609.11904).
- Noise2Fret: ISMIR 2026, [arXiv 2608.30854](https://arxiv.org/abs/2608.30854).
- MIDI-to-Tab: Edwards et al., ISMIR 2024, [arXiv 2408.05024](https://arxiv.org/abs/2408.05024).
- Fretting-Transformer: 2025, [arXiv 2506.14223](https://arxiv.org/abs/2506.14223).
- Note-event tokenization for MIDI-to-tab: 2026, [arXiv 2607.26440](https://arxiv.org/abs/2607.26440).
- SynthTab: Zang et al., ICASSP 2024, [arXiv 2309.09085](https://arxiv.org/abs/2309.09085).
- GOAT: Loth et al., ISMIR 2025, [arXiv 2509.22655](https://arxiv.org/abs/2509.22655).
- EGDB-PG / TIT: Chen et al., 2025, [arXiv 2504.07406](https://arxiv.org/abs/2504.07406).
- Real tones for GTT robustness: Pedroza et al., 2024, [arXiv 2405.14679](https://arxiv.org/abs/2405.14679).
- EG-VAE tone transfer and removal: 2026, [arXiv 2608.05513](https://arxiv.org/abs/2608.05513).
- GuitarFlow: CMMR 2025, [arXiv 2510.21872](https://arxiv.org/abs/2510.21872).
- Banquet: Watcharasupat & Lerch, ISMIR 2024, [arXiv 2406.18747](https://arxiv.org/abs/2406.18747).
- MoisesDB: Pereira et al., ISMIR 2023, [arXiv 2307.15913](https://arxiv.org/abs/2307.15913).
- MSST: 2026, [arXiv 2607.23395](https://arxiv.org/abs/2607.23395).
- Beat This!: Foscarin et al., ISMIR 2024, [arXiv 2407.21658](https://arxiv.org/abs/2407.21658);
  AlignBeat [arXiv 2510.14391](https://arxiv.org/abs/2510.14391);
  Explore This! [arXiv 2610.14817](https://arxiv.org/abs/2610.14817);
  SMC blind spot [arXiv 2605.12287](https://arxiv.org/abs/2605.12287).
