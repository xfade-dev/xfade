#!/usr/bin/env sh
# xfade one-line installer
#
#   curl -fsSL https://xfade.sh | sh
#
# Installs the latest (or a pinned) `xfade` CLI binary for macOS / Linux into
# $HOME/.local/bin (override with XFADE_INSTALL_DIR).
set -eu

REPO="xfade-dev/xfade"
BIN="xfade"
INSTALL_DIR="${XFADE_INSTALL_DIR:-$HOME/.local/bin}"

# --- detect platform -------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) target_os="apple-darwin" ;;
  Linux)  target_os="unknown-linux-gnu" ;;
  *)
    echo "xfade: unsupported OS '$os' (macOS / Linux only)" >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64|amd64) target_arch="x86_64" ;;
  arm64|aarch64) target_arch="aarch64" ;;
  *)
    echo "xfade: unsupported arch '$arch'" >&2
    exit 1
    ;;
esac

target="${target_arch}-${target_os}"
echo "xfade: installing for ${target}"

# --- resolve download URL --------------------------------------------------
asset="xfade-${target}.tar.gz"
if [ -n "${XFADE_VERSION:-}" ]; then
  base="https://github.com/${REPO}/releases/download/${XFADE_VERSION}"
else
  base="https://github.com/${REPO}/releases/latest/download"
fi
url="${base}/${asset}"
sums_url="${base}/SHA256SUMS"

# --- download + extract ----------------------------------------------------
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "xfade: downloading $url"
curl -fsSL "$url" -o "$tmpdir/xfade.tar.gz"

# --- verify integrity (fail closed) ---------------------------------------
# The tarball must match the SHA256SUMS published with the same release.
# Nothing is extracted or installed unless the checksum matches.
if ! curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS"; then
  echo "xfade: could not download $sums_url; refusing to install an unverified binary" >&2
  exit 1
fi
expected="$(awk -v a="$asset" '{ f=$2; sub(/^\*/, "", f); if (f == a) { print $1; exit } }' "$tmpdir/SHA256SUMS")"
if [ -z "$expected" ]; then
  echo "xfade: $asset is not listed in SHA256SUMS; refusing to install" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmpdir/xfade.tar.gz" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmpdir/xfade.tar.gz" | awk '{print $1}')"
elif command -v openssl >/dev/null 2>&1; then
  actual="$(openssl dgst -sha256 "$tmpdir/xfade.tar.gz" | awk '{print $NF}')"
else
  echo "xfade: no sha256sum/shasum/openssl available to verify the download" >&2
  exit 1
fi
if [ "$expected" != "$actual" ]; then
  echo "xfade: checksum mismatch for $asset" >&2
  echo "  expected: $expected" >&2
  echo "  actual:   $actual" >&2
  exit 1
fi
echo "xfade: checksum verified"

tar xzf "$tmpdir/xfade.tar.gz" -C "$tmpdir"

# --- install ---------------------------------------------------------------
# Prefer $INSTALL_DIR; if it isn't writable, fall back to /usr/local/bin (sudo).
if mkdir -p "$INSTALL_DIR" 2>/dev/null && [ -w "$INSTALL_DIR" ]; then
  install -m 0755 "$tmpdir/$BIN" "$INSTALL_DIR/$BIN"
else
  echo "xfade: $INSTALL_DIR is not writable; falling back to /usr/local/bin (may prompt for sudo)"
  sudo install -m 0755 "$tmpdir/$BIN" "/usr/local/bin/$BIN"
  INSTALL_DIR="/usr/local/bin"
fi

echo "xfade: installed -> $INSTALL_DIR/$BIN"

# --- PATH hint -------------------------------------------------------------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "xfade: add $INSTALL_DIR to your PATH:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
