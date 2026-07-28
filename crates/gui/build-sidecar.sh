#!/usr/bin/env bash
# 构建 asw 二进制并放到 Tauri sidecar 目录（按 host triple 命名）。
# 本地 `cargo tauri dev/build` 前运行；release workflow 也有等价步骤。
set -euo pipefail

TRIPLE=$(rustc -vV | awk '/^host/ {print $2}')
echo "host triple: $TRIPLE"

cargo build --release -p asw

mkdir -p crates/gui/src-tauri/binaries
BIN=target/release/asw
[ "$(uname)" = "Darwin" ] || [ "$(uname)" = "Linux" ] || BIN=target/release/asw.exe
cp "$BIN" "crates/gui/src-tauri/binaries/asw-$TRIPLE"
echo "sidecar -> crates/gui/src-tauri/binaries/asw-$TRIPLE"
