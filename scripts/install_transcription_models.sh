#!/usr/bin/env sh
# Installs the optional models that improve automatic tablature, where the
# app looks for them:
#   <data dir>/guitar_virtual_amp/tools/models/
# (Linux: ~/.local/share/..., macOS: ~/Library/Application Support/...).
#
# - Guitar transcription CRNN (Riley et al., GAPS, ISMIR 2024; MIT): the
#   PyTorch checkpoint from Hugging Face (~99 MB) is exported to ONNX by
#   scripts/export_guitar_crnn.py. Needs PyTorch: reuses the Demucs
#   environment when it exists, else a temporary CPU-only one made with
#   `uv` (https://docs.astral.sh/uv/).
# - Beat This! beat/downbeat tracker (Foscarin et al., ISMIR 2024; MIT):
#   ONNX files of the small0 checkpoint and its log-mel front end (~15 MB).
#
# Without these files the app falls back to Basic Pitch and the DSP beat
# tracker.
set -eu
case "$(uname -s)" in
  Darwin) DATA="$HOME/Library/Application Support" ;;
  *) DATA="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
esac
TOOLS="$DATA/guitar_virtual_amp/tools"
MODELS="$TOOLS/models"
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$MODELS"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# fetch URL DEST SHA256
fetch() {
  curl -fL --retry 3 -o "$2.part" "$1"
  got="$(sha256 "$2.part")"
  if [ "$got" != "$3" ]; then
    rm -f "$2.part"
    echo "checksum mismatch for $1 (got $got)" >&2
    exit 1
  fi
  mv "$2.part" "$2"
}

BEAT_REPO="https://huggingface.co/nilsbc/beat-this-onnx/resolve/0d66b58ee1b3b42ec5e82334949626295f3ed82b"
fetch "$BEAT_REPO/beat_this_small0.onnx" "$MODELS/beat_this_small0.onnx" \
  53fda6e96afae7f44d898fa5f020272eda967a92839306bf7a3d0898fb8a0d9a
fetch "$BEAT_REPO/logmel.onnx" "$MODELS/beat_this_logmel.onnx" \
  f5afa499f5a42b05ea89a214561170cd4b76f92d67d3227f7c9833e8d79bbb43
curl -fsL -o "$MODELS/BEAT_THIS_LICENSE" "$BEAT_REPO/LICENSE"

GUITAR_REPO="https://huggingface.co/xavriley/midi-transcription-models/resolve/b7bec65a2b860aca72856b0feef58b5df407b777"
fetch "$GUITAR_REPO/guitar-gaps.pth" "$TMP/guitar-gaps.pth" \
  65483e7c0e340a90415b15b520687587698c8c728f5fa470a205f13ee45c6513

PY="$TOOLS/demucs-venv/bin/python"
if [ -x "$PY" ] && "$PY" -c "import torch" >/dev/null 2>&1; then
  if ! "$PY" -c "import onnx" >/dev/null 2>&1; then
    if command -v uv >/dev/null 2>&1; then
      uv pip install --python "$PY" onnx
    else
      "$PY" -m pip install onnx
    fi
  fi
else
  if ! command -v uv >/dev/null 2>&1; then
    echo "uv not found: install it from https://docs.astral.sh/uv/ (or run install_demucs.sh first)" >&2
    exit 1
  fi
  uv venv --python 3.12 "$TMP/venv"
  PY="$TMP/venv/bin/python"
  uv pip install --python "$PY" "torch==2.5.1" --index-url https://download.pytorch.org/whl/cpu
  uv pip install --python "$PY" onnx
fi
"$PY" "$HERE/export_guitar_crnn.py" "$TMP/guitar-gaps.pth" "$MODELS/guitar_gaps_crnn.onnx.part"
mv "$MODELS/guitar_gaps_crnn.onnx.part" "$MODELS/guitar_gaps_crnn.onnx"
echo "Models installed in $MODELS"
