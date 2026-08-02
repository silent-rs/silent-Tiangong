#!/usr/bin/env bash
# 供 pre-commit pre-push hook 使用：只对受影响的 crate 执行 cargo 子命令。
#
# pre-commit 会把改动的文件路径作为参数传入（pass_filenames: true）。
# 本脚本从中提取 crate 名，去重后直接执行：
#   cargo <sub> -p <crate1> -p <crate2> ...
#
# 用法（由 .pre-commit-config.yaml 的 entry 调用）：
#   pre-commit-affected-crates.sh <check|clippy|nextest> [文件...]
set -euo pipefail

sub="${1:-check}"
shift || true

# 从文件路径提取 crate 名并去重：
#   crates/<name>/...           → <name>
#   crates/plugins/tiangong-plugin-memory/<part>/... → tiangong-plugin-memory-<part>
#   crates/plugins/<name>/...   → <name>（插件 crate 嵌套在 plugins/ 子目录下）
#   src-tauri/...               → tiangong-app
#   src/...                     → tiangong
# 用 if [[ ]] 模式匹配 + sort -u 去重，兼容老版本 bash，避免 case 在命令替换内的解析问题
crates="$(
  for f in "$@"; do
    if [[ "$f" == crates/plugins/tiangong-plugin-memory/protocol/* ]]; then
      printf '%s\n' "tiangong-plugin-memory-protocol"
    elif [[ "$f" == crates/plugins/tiangong-plugin-memory/sidecar/* ]]; then
      printf '%s\n' "tiangong-plugin-memory-sidecar"
    elif [[ "$f" == crates/plugins/tiangong-plugin-memory/wasm/* ]]; then
      printf '%s\n' "tiangong-plugin-memory-wasm"
    elif [[ "$f" == crates/plugins/tiangong-plugin-mcp/protocol/* ]]; then
      printf '%s\n' "tiangong-plugin-mcp-protocol"
    elif [[ "$f" == crates/plugins/tiangong-plugin-mcp/sidecar/* ]]; then
      printf '%s\n' "tiangong-plugin-mcp-sidecar"
    elif [[ "$f" == crates/plugins/tiangong-plugin-mcp/wasm/* ]]; then
      printf '%s\n' "tiangong-plugin-mcp-wasm"
    elif [[ "$f" == crates/plugins/tiangong-plugin-index/protocol/* ]]; then
      printf '%s\n' "tiangong-plugin-index-protocol"
    elif [[ "$f" == crates/plugins/tiangong-plugin-index/sidecar/* ]]; then
      printf '%s\n' "tiangong-plugin-index-sidecar"
    elif [[ "$f" == crates/plugins/tiangong-plugin-index/wasm/* ]]; then
      printf '%s\n' "tiangong-plugin-index-wasm"
    elif [[ "$f" == crates/plugins/*/* ]]; then
      rest="${f#crates/plugins/}"
      printf '%s\n' "${rest%%/*}"
    elif [[ "$f" == crates/*/* ]]; then
      rest="${f#crates/}"
      printf '%s\n' "${rest%%/*}"
    elif [[ "$f" == src-tauri/* ]]; then
      printf '%s\n' "tiangong-app"
    elif [[ "$f" == src/* ]]; then
      printf '%s\n' "tiangong"
    fi
  done | sort -u
)"

if [[ -z "$crates" ]]; then
  echo "pre-push[${sub}]: 无受影响 crate，跳过"
  exit 0
fi

echo "pre-push[${sub}]: 受影响 crate = $(echo "$crates" | tr '\n' ' ')"
# 拼成 -p a -p b ... 参数；跳过 workspace 中已不存在的包（如已删除的旧 crate，
# 其路径仍可能作为本次改动被 pre-commit 传入）。
args=()
while IFS= read -r c; do
  if cargo metadata --no-deps --format-version 1 >/dev/null 2>&1 &&
     grep -q "\"name\":\"$c\"" <(cargo metadata --no-deps --format-version 1 2>/dev/null); then
    args+=("-p" "$c")
  else
    echo "pre-push[${sub}]: 跳过不存在的 crate: $c"
  fi
done <<< "$crates"

if [[ ${#args[@]} -eq 0 ]]; then
  echo "pre-push[${sub}]: 受影响 crate 均不存在，跳过"
  exit 0
fi

case "$sub" in
  check)   cargo check "${args[@]}" ;;
  clippy)  cargo clippy "${args[@]}" --all-targets --tests --benches -- -D warnings ;;
  nextest) cargo nextest run "${args[@]}" --no-tests pass ;;
  *) echo "pre-push: 未知子命令 '$sub'（应为 check/clippy/nextest）" >&2; exit 1 ;;
esac
