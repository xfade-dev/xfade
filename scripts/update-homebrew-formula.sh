#!/usr/bin/env bash
# Update Homebrew formula and push to the standalone tap repo (xfade-dev/homebrew-xfade).
#
# Usage:
#   scripts/update-homebrew-formula.sh [vX.Y.Z]
#
# If no version is specified, it reads workspace.package.version from Cargo.toml.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TAG="${1:-}"
if [ -z "$TAG" ]; then
  VER="$(grep -m 1 '^version = ' Cargo.toml | cut -d '"' -f 2)"
  TAG="v${VER}"
fi

VER="${TAG#v}"
echo "==> Updating Homebrew formula for xfade ${TAG} (version ${VER})..."

SUMS_URL="https://github.com/xfade-dev/xfade/releases/download/${TAG}/SHA256SUMS"
echo "==> Fetching ${SUMS_URL}..."

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if ! curl -fsSL "$SUMS_URL" -o "$TMP/SHA256SUMS"; then
  echo "Error: Could not download SHA256SUMS from ${SUMS_URL}" >&2
  echo "Make sure the GitHub release for ${TAG} has been published and assets uploaded." >&2
  exit 1
fi

SHA_ARM64="$(awk '$2 == "xfade-aarch64-apple-darwin.tar.gz" {print $1}' "$TMP/SHA256SUMS")"
SHA_INTEL="$(awk '$2 == "xfade-x86_64-apple-darwin.tar.gz" {print $1}' "$TMP/SHA256SUMS")"
SHA_LINUX="$(awk '$2 == "xfade-x86_64-unknown-linux-gnu.tar.gz" {print $1}' "$TMP/SHA256SUMS")"

if [ -z "$SHA_ARM64" ] || [ -z "$SHA_INTEL" ]; then
  echo "Error: Could not find sha256 for macOS assets in SHA256SUMS:" >&2
  cat "$TMP/SHA256SUMS" >&2
  exit 1
fi

echo "  macOS ARM64 sha256: ${SHA_ARM64}"
echo "  macOS Intel sha256: ${SHA_INTEL}"
if [ -n "$SHA_LINUX" ]; then
  echo "  Linux x86_64 sha256: ${SHA_LINUX}"
fi

# 1. Update the local formula in this repository
FORMULA_PATH="homebrew/Formula/xfade.rb"
cat > "$FORMULA_PATH" <<EOF
class Xfade < Formula
  desc "Vendor-neutral, zero-raw-key gateway for AI coding CLIs"
  homepage "https://xfade.dev"
  license any_of: ["MIT", "Apache-2.0"]

  # This formula belongs to the standalone tap repo homebrew-xfade; url/sha256 are updated after each release.
  on_macos do
    on_arm do
      url "https://github.com/xfade-dev/xfade/releases/download/${TAG}/xfade-aarch64-apple-darwin.tar.gz"
      sha256 "${SHA_ARM64}"
    end
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/${TAG}/xfade-x86_64-apple-darwin.tar.gz"
      sha256 "${SHA_INTEL}"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/xfade-dev/xfade/releases/download/${TAG}/xfade-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${SHA_LINUX}"
    end
  end

  def install
    bin.install "xfade"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/xfade --version")
  end
end
EOF

echo "==> Updated local formula: ${FORMULA_PATH}"

# 2. Clone and update the standalone tap repo: homebrew-xfade
TAP_REPO="git@github.com:xfade-dev/homebrew-xfade.git"
TAP_DIR="$TMP/homebrew-xfade"

echo "==> Cloning tap repository: ${TAP_REPO}..."
git clone "$TAP_REPO" "$TAP_DIR"

mkdir -p "$TAP_DIR/Formula"
cp "$FORMULA_PATH" "$TAP_DIR/Formula/xfade.rb"

cd "$TAP_DIR"
git config user.name "xfade team"
git config user.email "core@xfade.dev"

if git diff --quiet; then
  echo "==> Tap repository is already up to date with ${TAG}."
else
  git commit -am "${TAG}"
  echo "==> Pushing updated formula to ${TAP_REPO}..."
  git push origin master
  echo "==> Tap repository updated successfully!"
fi

echo ""
echo "==> Homebrew update complete! Users can upgrade via:"
echo "      brew update && brew upgrade xfade-dev/xfade/xfade"
