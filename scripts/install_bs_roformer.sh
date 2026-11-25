#!/usr/bin/env sh
# Installs the BS-RoFormer SW source separator (6 stems: bass, drums, other,
# vocals, guitar, piano), which the app prefers over Demucs when it's there:
#   <data dir>/guitar_virtual_amp/tools/msst-venv/            Python + MSST code
#   <data dir>/guitar_virtual_amp/tools/models/bs_roformer_sw/ checkpoint (~700 MB)
# (Linux: ~/.local/share/..., macOS: ~/Library/Application Support/...).
#
# - Code: ZFTurbo's Music-Source-Separation-Training (MIT,
#   https://github.com/ZFTurbo/Music-Source-Separation-Training), pinned
#   commit, with only what bs_roformer inference needs (not the training
#   stack), plus scripts/separate_msst.py, the runner the app calls.
# - Checkpoint: "BS-RoFormer SW" from the Hugging Face mirror
#   FractalSurfer/BS-RoFormer-SW-6stem. ITS LICENSE IS UNKNOWN: the mirror
#   states none. It is downloaded for local, personal use only; never bundle
#   or redistribute it with the app.
#
# Needs `uv` (https://docs.astral.sh/uv/) and git. Pass --cpu to skip the
# CUDA build of PyTorch (smaller download; separation is then several times
# slower). On the GPU it needs about 4 GB of free memory.
set -eu
case "$(uname -s)" in
  Darwin) DATA="$HOME/Library/Application Support" ;;
  *) DATA="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
esac
TOOLS="$DATA/guitar_virtual_amp/tools"
VENV="$TOOLS/msst-venv"
MODEL_DIR="$TOOLS/models/bs_roformer_sw"
HERE="$(cd "$(dirname "$0")" && pwd)"
MSST_REPO="https://github.com/ZFTurbo/Music-Source-Separation-Training"
MSST_COMMIT="1523206d12a521bdb20673726562524cf0f9635b"
CKPT_REPO="https://huggingface.co/FractalSurfer/BS-RoFormer-SW-6stem/resolve/2b98254b19bdab07ce225d884ec4aa9dd8aafce1"

for tool in uv git curl; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool not found (uv: https://docs.astral.sh/uv/)" >&2
    exit 1
  fi
done
# The CUDA wheels are large; uv's default 30 s read timeout fails on slow
# mirrors.
export UV_HTTP_TIMEOUT="${UV_HTTP_TIMEOUT:-300}"
INDEX="https://download.pytorch.org/whl/cu126"
if [ "${1:-}" = "--cpu" ] || [ "$(uname -s)" = "Darwin" ]; then
  INDEX="https://download.pytorch.org/whl/cpu"
fi

echo "BS-RoFormer SW: the checkpoint's license is unknown (the mirror states none)."
echo "It is installed for local, personal use only. Don't redistribute it."

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
# fetch URL DEST SHA256 (skipped when DEST already has that checksum)
fetch() {
  if [ -f "$2" ] && [ "$(sha256 "$2")" = "$3" ]; then
    echo "already downloaded: $2"
    return
  fi
  curl -fL --retry 3 -C - -o "$2.part" "$1"
  got="$(sha256 "$2.part")"
  if [ "$got" != "$3" ]; then
    rm -f "$2.part"
    echo "checksum mismatch for $1 (got $got)" >&2
    exit 1
  fi
  mv "$2.part" "$2"
}

mkdir -p "$TOOLS" "$MODEL_DIR"
fetch "$CKPT_REPO/BS-Rofo-SW-Fixed.yaml" "$MODEL_DIR/BS-Rofo-SW-Fixed.yaml" \
  f9fada9f94e5ba2d2e4600196299459294bc5f532b314c209cc156ac63e4329b
fetch "$CKPT_REPO/BS-Rofo-SW-Fixed.ckpt" "$MODEL_DIR/BS-Rofo-SW-Fixed.ckpt" \
  24e7d35ee9c64415673d3fd33e06a67cac2c103c5df6267ba1576459c775916e
cat > "$MODEL_DIR/LICENSE_UNKNOWN.txt" <<EOF
BS-RoFormer SW checkpoint, from $CKPT_REPO
No license is stated for these weights. Downloaded for local, personal use
only by scripts/install_bs_roformer.sh; don't redistribute them.
EOF

if [ ! -x "$VENV/bin/python" ]; then
  uv venv --python 3.12 "$VENV"
fi
uv pip install --python "$VENV/bin/python" "torch==2.13.0" --index-url "$INDEX"
# What models/bs_roformer and utils/{settings,model_utils}.py import, with
# MSST's pins where it has them (requirements.txt pulls in the training stack).
uv pip install --python "$VENV/bin/python" numpy soundfile pyyaml packaging tqdm \
  ml_collections "omegaconf==2.2.3" "beartype==0.14.1" "rotary_embedding_torch==0.3.5" \
  "einops==0.8.1" librosa

SRC="$VENV/src/msst"
if [ ! -d "$SRC/.git" ]; then
  rm -rf "$SRC"
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin "$MSST_REPO"
fi
git -C "$SRC" fetch -q --depth 1 origin "$MSST_COMMIT"
git -C "$SRC" checkout -q --detach "$MSST_COMMIT"
cp "$HERE/separate_msst.py" "$VENV/separate_msst.py"

"$VENV/bin/python" -c "
import sys, torch
sys.path.insert(0, '$SRC')
from utils.settings import get_model_from_config
get_model_from_config('bs_roformer', '$MODEL_DIR/BS-Rofo-SW-Fixed.yaml')
print('PyTorch', torch.__version__, 'GPU:', torch.cuda.is_available())
"
echo "BS-RoFormer SW installed: $VENV (checkpoint in $MODEL_DIR)"
