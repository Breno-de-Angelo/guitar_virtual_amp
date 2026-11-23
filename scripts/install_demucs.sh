#!/usr/bin/env sh
# Installs Demucs (MIT, https://github.com/facebookresearch/demucs) in a
# private Python environment where the app looks for it:
#   <data dir>/guitar_virtual_amp/tools/demucs-venv/bin/demucs
# (Linux: ~/.local/share/..., macOS: ~/Library/Application Support/...).
# Needs `uv` (https://docs.astral.sh/uv/). Pass --cpu to skip the CUDA build
# of PyTorch (smaller download; separation is then several times slower).
# The htdemucs_6s model (~55 MB) is downloaded on the first separation.
set -eu

case "$(uname -s)" in
  Darwin) DATA="$HOME/Library/Application Support" ;;
  *) DATA="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
esac
VENV="$DATA/guitar_virtual_amp/tools/demucs-venv"

if ! command -v uv >/dev/null 2>&1; then
  echo "uv not found: install it from https://docs.astral.sh/uv/" >&2
  exit 1
fi

INDEX="https://download.pytorch.org/whl/cu124"
if [ "${1:-}" = "--cpu" ] || [ "$(uname -s)" = "Darwin" ]; then
  INDEX="https://download.pytorch.org/whl/cpu"
fi

mkdir -p "$(dirname "$VENV")"
# Python 3.12 + torch/torchaudio 2.5: demucs 4.0.1 saves audio through
# torchaudio APIs that newer torchaudio releases removed.
uv venv --python 3.12 "$VENV"
uv pip install --python "$VENV/bin/python" "torch==2.5.1" "torchaudio==2.5.1" --index-url "$INDEX"
uv pip install --python "$VENV/bin/python" "demucs==4.0.1" soundfile
"$VENV/bin/python" -c "import torch; print('PyTorch', torch.__version__, 'GPU:', torch.cuda.is_available())"
echo "Demucs installed: $VENV/bin/demucs"
