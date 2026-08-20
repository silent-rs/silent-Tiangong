#!/usr/bin/env bash
# 构建终端 sidecar 并复制到 release/（当前平台）。
# 二进制名由 Cargo.toml 的 [[bin]] 固定为 plugin.json 声明的
# tiangong-terminal-sidecar；制品只进 release/（随插件包分发），
# 不在工程目录留存。
set -euo pipefail
cd "$(dirname "$0")/../sidecar"
cargo build --release
binary="tiangong-terminal-sidecar"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) binary="${binary}.exe" ;;
esac
target_dir="$(cargo metadata --format-version 1 | python3 -c "import json,sys; print(json.load(sys.stdin)['target_directory'])")"
mkdir -p ../release
cp "${target_dir}/release/${binary}" "../release/${binary}"
echo "sidecar 已复制到 release/: ${binary}"
