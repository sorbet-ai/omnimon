#!/usr/bin/env bash
# Regenerate docs/img/*.png from the demo backend (needs WSL/Linux with tmux,
# Python + Pillow, and a font with braille glyphs, e.g. DejaVu Sans Mono).
#   FONT=/path/DejaVuSansMono.ttf scripts/screenshots.sh
set -euo pipefail
cd "$(dirname "$0")/.."
FONT=${FONT:-/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf}
PY=${PY:-python3}
mkdir -p docs/img
shot() { # name cols rows wait args keys...
  local name=$1; shift
  ANSI=1 scripts/tui-shot.sh "$@" > "/tmp/$name.ans"
  $PY scripts/ansi2png.py "/tmp/$name.ans" "docs/img/$name.png" "$FONT"
}
shot overview 140 44 30 "--demo 2 -i 0.25 --theme neon"
shot cluster 140 40 20 "--demo 8 -i 0.25 --theme catppuccin"
shot charts 150 42 30 "--demo 1 -i 0.25 --theme neon" 2
shot details 140 44 20 "--demo 3 -i 0.25 --theme dracula" 3
