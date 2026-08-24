#!/usr/bin/env bash
# 构建沙箱程序并放置为 Tauri externalBin 制品（开发与 CI 构建前运行）。
# Windows 构建在 Git Bash/WSL 下运行本脚本（tauri 官方构建链即如此）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p tiangong-sandbox --release
TRIPLE=$(rustc -vV | grep ^host: | cut -d' ' -f2)
case "$TRIPLE" in
  *windows*) SRC=target/release/tiangong-sandbox.exe ;;
  *) SRC=target/release/tiangong-sandbox ;;
esac
mkdir -p src-tauri/binaries
cp "$SRC" "src-tauri/binaries/tiangong-sandbox-$TRIPLE"
echo "已放置 src-tauri/binaries/tiangong-sandbox-$TRIPLE"
