#!/usr/bin/env bash
# Dev helper: copy the source tree to a Mac and build/run there.
#   MAC=user@host scripts/mac-sync.sh [command...]
set -euo pipefail
cd "$(dirname "$0")/.."
MAC=${MAC:?set MAC=user@host}
DIR=${MAC_DIR:-nvtui-dev}
SSH="ssh -o BatchMode=yes -o ConnectTimeout=8 $MAC"
tar czf - Cargo.toml Cargo.lock .cargo src scripts | $SSH "mkdir -p ~/$DIR && cd ~/$DIR && rm -rf src && tar xzf - 2>/dev/null"
$SSH "cd ~/$DIR && source ~/.cargo/env 2>/dev/null; cargo build --release 2>&1 | grep -E '^(warning: unused|error)|-->|Finished' | head -40; ${*:-true}"
