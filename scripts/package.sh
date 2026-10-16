#!/usr/bin/env bash
# Generate package-manager manifests for a published GitHub release from its
# SHA256SUMS: Homebrew formula, Scoop manifest, winget manifests, AUR PKGBUILD.
#
#   scripts/package.sh 0.1.2              # writes packaging/out/
#   scripts/package.sh 0.1.2 --publish    # also pushes the Homebrew tap and
#                                         # Scoop bucket repos (needs gh auth)
#
# winget and AUR need a submission from a maintainer account; the script
# prints how.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=${1:?usage: scripts/package.sh VERSION [--publish]}
VERSION=${VERSION#v}
PUBLISH=${2:-}
REPO=sorbet-ai/omnimon
TAP=sorbet-ai/homebrew-tap
BUCKET=sorbet-ai/scoop-bucket
URL="https://github.com/$REPO/releases/download/v$VERSION"
DESC="Terminal monitor for GPUs, the processes using them and the machine around them"
OUT=packaging/out
rm -rf "$OUT" && mkdir -p "$OUT"/{homebrew,scoop,aur} "$OUT/winget/$VERSION"

if [ -n "${SUMS:-}" ]; then # a local SHA256SUMS, for testing
  cp "$SUMS" "$OUT/SHA256SUMS"
else
  curl -fsSL "$URL/SHA256SUMS" -o "$OUT/SHA256SUMS" || gh release download "v$VERSION" -R "$REPO" -p SHA256SUMS -D "$OUT"
fi
sum() { awk -v f="$1" '$2 == f || $2 == "*"f { print $1 }' "$OUT/SHA256SUMS" | grep . || { echo "missing $1 in SHA256SUMS" >&2; exit 1; }; }
MAC_ARM=$(sum omnimon-aarch64-apple-darwin)
MAC_X86=$(sum omnimon-x86_64-apple-darwin)
LINUX_ARM=$(sum omnimon-aarch64-unknown-linux-gnu)
LINUX_X86=$(sum omnimon-x86_64-unknown-linux-gnu)
WIN_X86=$(sum omnimon-x86_64-pc-windows-msvc.exe)

cat > "$OUT/homebrew/omnimon.rb" <<EOF
class Omnimon < Formula
  desc "$DESC"
  homepage "https://github.com/$REPO"
  version "$VERSION"
  license "MIT"

  on_macos do
    on_arm do
      url "$URL/omnimon-aarch64-apple-darwin"
      sha256 "$MAC_ARM"
    end
    on_intel do
      url "$URL/omnimon-x86_64-apple-darwin"
      sha256 "$MAC_X86"
    end
  end
  on_linux do
    on_arm do
      url "$URL/omnimon-aarch64-unknown-linux-gnu"
      sha256 "$LINUX_ARM"
    end
    on_intel do
      url "$URL/omnimon-x86_64-unknown-linux-gnu"
      sha256 "$LINUX_X86"
    end
  end

  def install
    bin.install Dir["omnimon-*"].first => "omnimon"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/omnimon --version")
  end
end
EOF

cat > "$OUT/scoop/omnimon.json" <<EOF
{
    "version": "$VERSION",
    "description": "$DESC",
    "homepage": "https://github.com/$REPO",
    "license": "MIT",
    "architecture": {
        "64bit": {
            "url": "$URL/omnimon-x86_64-pc-windows-msvc.exe#/omnimon.exe",
            "hash": "$WIN_X86"
        }
    },
    "bin": "omnimon.exe",
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$REPO/releases/download/v\$version/omnimon-x86_64-pc-windows-msvc.exe#/omnimon.exe"
            }
        },
        "hash": {
            "url": "https://github.com/$REPO/releases/download/v\$version/SHA256SUMS",
            "regex": "\$sha256\\\\s+\\\\*?omnimon-x86_64-pc-windows-msvc\\\\.exe"
        }
    }
}
EOF

W="$OUT/winget/$VERSION"
ID=SorbetAI.omnimon
cat > "$W/$ID.yaml" <<EOF
PackageIdentifier: $ID
PackageVersion: $VERSION
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
EOF
cat > "$W/$ID.installer.yaml" <<EOF
PackageIdentifier: $ID
PackageVersion: $VERSION
InstallerType: portable
Commands:
  - omnimon
Installers:
  - Architecture: x64
    InstallerUrl: $URL/omnimon-x86_64-pc-windows-msvc.exe
    InstallerSha256: $(echo "$WIN_X86" | tr a-f A-F)
ManifestType: installer
ManifestVersion: 1.6.0
EOF
cat > "$W/$ID.locale.en-US.yaml" <<EOF
PackageIdentifier: $ID
PackageVersion: $VERSION
PackageLocale: en-US
Publisher: Sorbet AI
PackageName: omnimon
PackageUrl: https://github.com/$REPO
License: MIT
LicenseUrl: https://github.com/$REPO/blob/main/LICENSE
ShortDescription: $DESC
Tags:
  - gpu
  - monitor
  - nvidia
  - amd
  - terminal
ManifestType: defaultLocale
ManifestVersion: 1.6.0
EOF

cat > "$OUT/aur/PKGBUILD" <<EOF
pkgname=omnimon-bin
pkgver=$VERSION
pkgrel=1
pkgdesc="$DESC"
arch=('x86_64' 'aarch64')
url="https://github.com/$REPO"
license=('MIT')
depends=('glibc')
provides=('omnimon')
conflicts=('omnimon')
source_x86_64=("omnimon-\$pkgver-x86_64::\$url/releases/download/v\$pkgver/omnimon-x86_64-unknown-linux-gnu")
source_aarch64=("omnimon-\$pkgver-aarch64::\$url/releases/download/v\$pkgver/omnimon-aarch64-unknown-linux-gnu")
sha256sums_x86_64=('$LINUX_X86')
sha256sums_aarch64=('$LINUX_ARM')

package() {
  install -Dm755 "\$srcdir/omnimon-\$pkgver-\$CARCH" "\$pkgdir/usr/bin/omnimon"
}
EOF
cat > "$OUT/aur/.SRCINFO" <<EOF
pkgbase = omnimon-bin
	pkgdesc = $DESC
	pkgver = $VERSION
	pkgrel = 1
	url = https://github.com/$REPO
	arch = x86_64
	arch = aarch64
	license = MIT
	depends = glibc
	provides = omnimon
	conflicts = omnimon
	source_x86_64 = omnimon-$VERSION-x86_64::https://github.com/$REPO/releases/download/v$VERSION/omnimon-x86_64-unknown-linux-gnu
	sha256sums_x86_64 = $LINUX_X86
	source_aarch64 = omnimon-$VERSION-aarch64::https://github.com/$REPO/releases/download/v$VERSION/omnimon-aarch64-unknown-linux-gnu
	sha256sums_aarch64 = $LINUX_ARM

pkgname = omnimon-bin
EOF

echo "wrote $OUT/"
if [ "$PUBLISH" != --publish ]; then
  exit 0
fi

push() { # repo path-in-repo file
  local dir; dir=$(mktemp -d)
  gh repo clone "$1" "$dir" -- -q
  mkdir -p "$dir/$(dirname "$2")"
  cp "$3" "$dir/$2"
  git -C "$dir" add "$2"
  if git -C "$dir" diff --cached --quiet; then
    echo "$1: already up to date"
  else
    git -C "$dir" commit -q -m "omnimon $VERSION"
    git -C "$dir" push -q
    echo "$1: updated to $VERSION"
  fi
  rm -rf "$dir"
}
push "$TAP" Formula/omnimon.rb "$OUT/homebrew/omnimon.rb"
push "$BUCKET" bucket/omnimon.json "$OUT/scoop/omnimon.json"
cat <<EOF

winget: submit $OUT/winget/$VERSION as manifests/s/SorbetAI/omnimon/$VERSION in a
pull request to microsoft/winget-pkgs (or: wingetcreate submit $OUT/winget/$VERSION).
AUR: commit $OUT/aur/{PKGBUILD,.SRCINFO} to ssh://aur@aur.archlinux.org/omnimon-bin.git
EOF
