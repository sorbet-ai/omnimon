#!/usr/bin/env bash
# Headless TUI capture for development: runs the Linux binary in a detached
# tmux session of a given size, sends keys, and prints the screen.
#   scripts/tui-shot.sh <cols> <rows> <wait-secs> "<args>" [keys...]
set -euo pipefail
COLS=$1 ROWS=$2 WAIT=$3 ARGS=$4; shift 4
BIN=$(cd "$(dirname "$0")/.." && pwd)/dist/$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$(dirname "$0")/../Cargo.toml" | head -1)-x86_64-linux
S=shot$$
tmux new-session -d -s $S -x "$COLS" -y "$ROWS" "COLORTERM=truecolor TERM=xterm-256color $BIN $ARGS"
sleep "$WAIT"
for k in "$@"; do tmux send-keys -t $S "$k"; sleep 0.6; done
tmux capture-pane -p -t $S ${ANSI:+-e}
tmux kill-session -t $S
