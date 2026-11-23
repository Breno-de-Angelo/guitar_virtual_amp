#!/usr/bin/env sh
# Installs yt-dlp (Unlicense, https://github.com/yt-dlp/yt-dlp) in a private
# Python environment where the app looks for it:
#   <data dir>/guitar_virtual_amp/tools/ytdlp-venv/bin/yt-dlp
# Needs `uv` (https://docs.astral.sh/uv/) and ffmpeg (audio extraction).
# YouTube changes often: rerun this script to update yt-dlp when downloads
# start failing.
#
# YouTube's Terms of Service only allow downloading where YouTube offers it
# or with permission from YouTube and the rights holders. Use it for content
# you have the right to download.
set -eu

case "$(uname -s)" in
  Darwin) DATA="$HOME/Library/Application Support" ;;
  *) DATA="${XDG_DATA_HOME:-$HOME/.local/share}" ;;
esac
VENV="$DATA/guitar_virtual_amp/tools/ytdlp-venv"

if ! command -v uv >/dev/null 2>&1; then
  echo "uv not found: install it from https://docs.astral.sh/uv/" >&2
  exit 1
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "warning: ffmpeg not found; yt-dlp needs it to extract the audio track" >&2
fi

mkdir -p "$(dirname "$VENV")"
[ -d "$VENV" ] || uv venv --python 3.12 "$VENV"
# [default] pulls yt-dlp-ejs, the JavaScript challenge solver YouTube now
# requires (it runs on deno or node, whichever is installed).
uv pip install --python "$VENV/bin/python" --upgrade "yt-dlp[default]"
"$VENV/bin/yt-dlp" --version
echo "yt-dlp installed: $VENV/bin/yt-dlp"
