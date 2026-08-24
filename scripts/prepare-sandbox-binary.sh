#!/usr/bin/env bash
# 构建沙箱程序并放置为 Tauri externalBin 制品（开发与 CI 构建前运行）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p tiangong-sandbox --release
TRIPLE=$(rustc -vV | grep ^host: | cut -d' ' -f2)
mkdir -p src-tauri/binaries
cp "target/release/tiangong-sandbox" "src-tauri/binaries/tiangong-sandbox-$TRIPLE"
echo "已放置 src-tauri/binaries/tiangong-sandbox-$TRIPLE"
