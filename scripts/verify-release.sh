#!/usr/bin/env sh
# Download and verify an xfade CLI release using both SHA-256 and its
# keyless Sigstore signature.
#
# Usage:
#   scripts/verify-release.sh v0.11.0
#   scripts/verify-release.sh v0.11.0 aarch64-apple-darwin
#
# Requires: curl, cosign, and sha256sum (Linux) or shasum (macOS).
set -eu

REPO="xfade-dev/xfade"
VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "usage: $0 vX.Y.Z [target]" >&2
  exit 2
fi
case "$VERSION" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "xfade: expected a release tag such as v0.11.0, got '$VERSION'" >&2
    exit 2
    ;;
esac

if ! command -v cosign >/dev/null 2>&1; then
  echo "xfade: cosign is required: https://docs.sigstore.dev/cosign/system_config/installation/" >&2
  exit 2
fi

if [ -n "${2:-}" ]; then
  target="$2"
else
  case "$(uname -s)" in
    Darwin) target_os="apple-darwin" ;;
    Linux) target_os="unknown-linux-gnu" ;;
    *)
      echo "xfade: specify the release target explicitly on this OS" >&2
      exit 2
      ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) target_arch="x86_64" ;;
    arm64|aarch64) target_arch="aarch64" ;;
    *)
      echo "xfade: unsupported architecture: $(uname -m)" >&2
      exit 2
      ;;
  esac
  target="${target_arch}-${target_os}"
fi

case "$target" in
  *-pc-windows-msvc) asset="xfade-${target}.zip" ;;
  *) asset="xfade-${target}.tar.gz" ;;
esac
base="https://github.com/${REPO}/releases/download/${VERSION}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "xfade: downloading ${VERSION} verification material"
curl -fsSL "${base}/${asset}" -o "${tmpdir}/${asset}"
curl -fsSL "${base}/SHA256SUMS" -o "${tmpdir}/SHA256SUMS"
curl -fsSL "${base}/SHA256SUMS.sigstore.json" -o "${tmpdir}/SHA256SUMS.sigstore.json"

expected="$(awk -v a="$asset" '{ f=$2; sub(/^\*/, "", f); if (f == a) { print $1; exit } }' "${tmpdir}/SHA256SUMS")"
if [ -z "$expected" ]; then
  echo "xfade: ${asset} is not listed in SHA256SUMS" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${tmpdir}/${asset}" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "${tmpdir}/${asset}" | awk '{print $1}')"
fi
if [ "$expected" != "$actual" ]; then
  echo "xfade: checksum mismatch for ${asset}" >&2
  exit 1
fi

cosign verify-blob \
  --bundle "${tmpdir}/SHA256SUMS.sigstore.json" \
  --certificate-identity "https://github.com/${REPO}/.github/workflows/release.yml@refs/tags/${VERSION}" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  "${tmpdir}/SHA256SUMS"

echo "xfade: verified ${asset}"
echo "  sha256: ${actual}"
echo "  signer: https://github.com/${REPO}/.github/workflows/release.yml@refs/tags/${VERSION}"
