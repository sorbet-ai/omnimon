#!/usr/bin/env bash
# Build the Windows release into dist/. A running copy is locked against
# overwriting but can be renamed, so it is moved aside first.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=$(sed -n 's/^name *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
mkdir -p dist/old
OUT="dist/$NAME-x86_64-windows.exe"
if [ -f "$OUT" ]; then
  mv "$OUT" "dist/old/$NAME-$(date +%Y%m%d-%H%M%S).exe"
fi
cp "target/release/$NAME.exe" "$OUT"
rm -f dist/old/*.exe 2>/dev/null || true   # succeeds for copies no longer running
echo "built $OUT"
