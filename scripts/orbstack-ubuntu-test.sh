#!/usr/bin/env bash
set -euo pipefail

VM_NAME="${ORB_VM_NAME:-rust-ubuntu-complier}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${ORB_CARGO_TARGET_DIR:-target/orbstack-linux}"
TEST_TIMEOUT="${ORB_TEST_TIMEOUT:-3600s}"
CARGO_BUILD_JOBS="${ORB_CARGO_BUILD_JOBS:-1}"

LINUX_DEPS=(
  protobuf-compiler
  pkg-config
  libglib2.0-dev
  libwebkit2gtk-4.1-dev
  libgtk-3-dev
  libayatana-appindicator3-dev
  librsvg2-dev
  patchelf
)

quote() {
  printf "%q" "$1"
}

run_in_vm() {
  local command="$1"

  orb -m "$VM_NAME" sh -lc "cd $(quote "$REPO_ROOT") && timeout $(quote "$TEST_TIMEOUT") bash -lc $(quote "$command")"
}

if ! command -v orb >/dev/null 2>&1; then
  echo "未找到 orb 命令，请先安装并启动 OrbStack。" >&2
  exit 127
fi

case "${1:-}" in
  --install-deps)
    run_in_vm "sudo apt-get update && sudo apt-get install -y ${LINUX_DEPS[*]}"
    ;;
  --help|-h)
    cat <<EOF
用法:
  scripts/orbstack-ubuntu-test.sh [cargo 命令]
  scripts/orbstack-ubuntu-test.sh --install-deps

默认命令:
  cargo nextest run --workspace --no-tests pass

可选环境变量:
  ORB_VM_NAME            默认 rust-ubuntu-complier
  ORB_CARGO_TARGET_DIR   默认 target/orbstack-linux
  ORB_CARGO_BUILD_JOBS   默认 1
  ORB_TEST_TIMEOUT       默认 3600s
EOF
    ;;
  *)
    if [[ $# -gt 0 ]]; then
      TEST_CMD="$*"
    else
      TEST_CMD="cargo nextest run --workspace --no-tests pass"
    fi

    run_in_vm "env CARGO_TARGET_DIR=$(quote "$TARGET_DIR") CARGO_BUILD_JOBS=$(quote "$CARGO_BUILD_JOBS") RUSTFLAGS='-C linker-features=-lld' $TEST_CMD"
    ;;
esac
