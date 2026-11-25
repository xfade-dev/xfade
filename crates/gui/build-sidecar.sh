#!/usr/bin/env bash
# Build the xfade binary and place it in the Tauri sidecar directory (named by host triple).
# Run before local `cargo tauri dev/build`; the release workflow has an equivalent step.
set -euo pipefail

TRIPLE=$(rustc -vV | awk '/^host/ {print $2}')
echo "host triple: $TRIPLE"

cargo build --release -p xfade

mkdir -p crates/gui/src-tauri/binaries
BIN=target/release/xfade
EXT=""
if [ "$(uname)" != "Darwin" ] && [ "$(uname)" != "Linux" ]; then
  BIN=target/release/xfade.exe
  EXT=".exe"
fi
cp "$BIN" "crates/gui/src-tauri/binaries/xfade-$TRIPLE$EXT"
echo "sidecar -> crates/gui/src-tauri/binaries/xfade-$TRIPLE$EXT"
