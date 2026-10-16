#!/usr/bin/env bash
# Build portable Linux binaries (glibc >= 2.17, i.e. basically any distro from
# the last decade) using cargo-zigbuild in Docker. Output: dist/<name>-<arch>-linux
#
#   scripts/build-linux.sh            # x86_64 + aarch64, release
#   scripts/build-linux.sh x86_64     # just one arch
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=$(sed -n 's/^name *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
ARCHS=${*:-x86_64 aarch64}
mkdir -p dist
for arch in $ARCHS; do
  target="$arch-unknown-linux-gnu"
  MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$PWD":/io -w /io \
    -v "${NAME}-cargo-registry":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/io/target/zig \
    messense/cargo-zigbuild \
    sh -c "rustup target add $target >/dev/null 2>&1; cargo zigbuild --release --target $target.2.17"
  cp "target/zig/$target/release/$NAME" "dist/$NAME-$arch-linux"
  echo "built dist/$NAME-$arch-linux"
done
