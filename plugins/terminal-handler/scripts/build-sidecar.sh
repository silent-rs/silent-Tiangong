#!/usr/bin/env bash
# 构建终端 sidecar 并复制到插件目录（当前平台）。
set -euo pipefail
cd "$(dirname "$0")/../sidecar"
cargo build --release
binary="tiangong-terminal-handler-sidecar"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) binary="${binary}.exe" ;;
esac
target_dir="$(cargo metadata --format-version 1 | python3 -c "import json,sys; print(json.load(sys.stdin)['target_directory'])")"
cp "${target_dir}/release/${binary}" "../terminal-handler-sidecar"
echo "sidecar 已复制到插件目录: ${binary}"
