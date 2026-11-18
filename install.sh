#!/bin/sh
# Install omnimon from the GitHub release for this OS and CPU.
#
#   curl -fsSL https://github.com/sorbet-ai/omnimon/releases/latest/download/install.sh | sh
#
# The latest build of main (rebuilt on every push):
#
#   curl -fsSL https://github.com/sorbet-ai/omnimon/releases/download/nightly/install.sh | sh
#
# Environment:
#   OMNIMON_VERSION        release to install: e.g. 0.1.1, "latest" or
#                          "nightly" (default: latest)
#   OMNIMON_INSTALL_DIR    where to put the binary (default: /usr/local/bin as
#                          root, otherwise ~/.local/bin)
#   OMNIMON_NO_MODIFY_PATH set to 1 to leave shell startup files alone when the
#                          install directory isn't on PATH
#   OMNIMON_DOWNLOAD_URL   base URL to download from instead of GitHub (mirrors,
#                          testing); files are expected as <base>/<asset>
set -eu

REPO="sorbet-ai/omnimon"
NAME="omnimon"
# The nightly release ships a copy of this script with this set to "nightly".
DEFAULT_VERSION="latest"

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

version="${OMNIMON_VERSION:-$DEFAULT_VERSION}"
if [ -n "${OMNIMON_DOWNLOAD_URL:-}" ]; then
  base="${OMNIMON_DOWNLOAD_URL%/}"
elif [ "$version" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
elif [ "$version" = nightly ]; then
  base="https://github.com/$REPO/releases/download/nightly"
else
  base="https://github.com/$REPO/releases/download/v${version#v}"
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

# Put the install directory on PATH for new shells, like the Windows
# installer does. This script runs in a child shell, so the shell that ran it
# can't be changed from here.
add_to_path() { # dir; prints the files it changed
  line="export PATH=\"$1:\$PATH\""
  for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    # ~/.profile is read by login shells (ssh) of any kind; the others only
    # matter when that shell is in use.
    [ "$rc" = "$HOME/.profile" ] || [ -f "$rc" ] || continue
    grep -qsF "$line" "$rc" && continue
    printf '\n# added by the %s installer\n%s\n' "$NAME" "$line" >>"$rc" 2>/dev/null && printf ' %s' "$rc"
  done
  if [ -d "$HOME/.config/fish" ]; then
    f="$HOME/.config/fish/conf.d/$NAME.fish"
    if ! grep -qsF "$1" "$f" && mkdir -p "${f%/*}" 2>/dev/null && printf 'fish_add_path %s\n' "$1" >"$f" 2>/dev/null; then
      printf ' %s' "$f"
    fi
  fi
}

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    if [ -z "${OMNIMON_NO_MODIFY_PATH:-}" ]; then
      changed="$(add_to_path "$dir")"
      [ -n "$changed" ] && say "added $dir to PATH in:$changed"
    fi
    say "$dir is not on this shell's PATH yet; to use $NAME now, run:"
    say "  export PATH=\"$dir:\$PATH\""
    ;;
esac
