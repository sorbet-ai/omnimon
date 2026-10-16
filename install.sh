#!/bin/sh
# Install omnimon from the GitHub release for this OS and CPU.
#
#   curl -fsSL https://github.com/sorbet-ai/omnimon/releases/latest/download/install.sh | sh
#
# Environment:
#   OMNIMON_VERSION      release to install, e.g. 0.1.1 (default: latest)
#   OMNIMON_INSTALL_DIR  where to put the binary (default: /usr/local/bin as
#                        root, otherwise ~/.local/bin)
#   OMNIMON_DOWNLOAD_URL base URL to download from instead of GitHub (mirrors,
#                        testing); files are expected as <base>/<asset>
set -eu

REPO="sorbet-ai/omnimon"
NAME="omnimon"

say() { printf '%s\n' "$*" >&2; }
fail() { say "error: $*"; exit 1; }

case "$(uname -s)" in
  Linux) os=unknown-linux-gnu ;;
  Darwin) os=apple-darwin ;;
  *) fail "unsupported OS $(uname -s); on Windows run: irm https://github.com/$REPO/releases/latest/download/install.ps1 | iex" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "unsupported CPU $(uname -m)" ;;
esac
# Apple Silicon running this shell under Rosetta still wants the native build.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
# The Linux build needs glibc (it loads the GPU driver's libraries).
if [ "$os" = unknown-linux-gnu ] && ldd --version 2>&1 | grep -qi musl; then
  fail "this looks like a musl system (e.g. Alpine); omnimon needs glibc 2.17 or newer"
fi
asset="$NAME-$arch-$os"

if [ -n "${OMNIMON_DOWNLOAD_URL:-}" ]; then
  base="${OMNIMON_DOWNLOAD_URL%/}"
elif [ -n "${OMNIMON_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/v${OMNIMON_VERSION#v}"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

if [ -n "${OMNIMON_INSTALL_DIR:-}" ]; then
  dir="$OMNIMON_INSTALL_DIR"
elif [ "$(id -u)" = 0 ]; then
  dir=/usr/local/bin
else
  dir="$HOME/.local/bin"
fi

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    fail "need curl or wget"
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset"
fetch "$base/$asset" "$tmp/$NAME" || fail "download failed: $base/$asset"

# Verify against the release's checksums when they're available.
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
  want="$(awk -v f="$asset" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS")"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$tmp/$NAME" | awk '{ print $1 }')"
  else
    got="$(shasum -a 256 "$tmp/$NAME" | awk '{ print $1 }')"
  fi
  [ -n "$want" ] || fail "$asset is not listed in SHA256SUMS"
  [ "$want" = "$got" ] || fail "checksum mismatch for $asset"
else
  say "note: no SHA256SUMS found, skipping verification"
fi

chmod +x "$tmp/$NAME"
mkdir -p "$dir"
mv -f "$tmp/$NAME" "$dir/$NAME" 2>/dev/null || fail "can't write to $dir (set OMNIMON_INSTALL_DIR, or run as root)"
say "installed $("$dir/$NAME" --version) to $dir/$NAME"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) say "add $dir to your PATH, e.g.: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile" ;;
esac
