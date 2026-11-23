# Bundled models

## `basic_pitch_nmp.onnx`

Basic Pitch note/onset model (ICASSP 2022), from
https://github.com/spotify/basic-pitch (`basic_pitch/saved_models/icassp_2022/nmp.onnx`).
Copyright Spotify AB, licensed under the Apache License 2.0 (see `BASIC_PITCH_LICENSE`).
Unmodified. Used by `guitar_core::transcribe::basic_pitch` through `tract-onnx`.

Inputs: `[batch, 43844, 1]` mono audio at 22050 Hz (2 s windows).
Outputs: 0 = onset posteriors `[batch, 172, 88]`, 1 = note posteriors `[batch, 172, 88]`,
2 = contour `[batch, 172, 264]`. Pitch index 0 = MIDI 21 (A0).

## Optional models (not bundled)
Installed by `scripts/install_transcription_models.sh` into
`<data dir>/guitar_virtual_amp/tools/models/`; the pipeline falls back to Basic Pitch and the
DSP beat tracker without them.
- `guitar_gaps_crnn.onnx`: `guitar-gaps.pth` from
  https://huggingface.co/xavriley/midi-transcription-models (MIT), the high-resolution CRNN of
  Kong et al. (2021) trained for guitar on GAPS by Riley et al. (ISMIR 2024), exported with
  `scripts/export_guitar_crnn.py`. Input `[1, 160000]` 16 kHz audio; outputs onset, offset,
  frame and velocity `[1, 1001, 88]` at 100 fps.
- `beat_this_small0.onnx`, `beat_this_logmel.onnx`: Beat This! (Foscarin, Schlüter, Widmer,
  ISMIR 2024; MIT, CPJKU/beat_this), ONNX conversion from
  https://huggingface.co/nilsbc/beat-this-onnx. 22.05 kHz audio -> 128 log-mel bands at 50 fps
  -> beat and downbeat logits.
