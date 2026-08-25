#!/usr/bin/env bash

set -euo pipefail

sandbox_change=false
other_change=false

while IFS= read -r file; do
  [[ -z "$file" ]] && continue
  case "$file" in
    .gitignore|Cargo.toml|Cargo.lock|.github/workflows/ci.yml|.github/workflows/plugin-ci.yml|\
    crates/tiangong-toolkit/Cargo.toml|crates/tiangong-toolkit/src/lib.rs)
      # Shared metadata and the command path-policy companion may accompany a
      # Sandbox-only change, but do not make an unrelated change Sandbox-only.
      ;;
    .github/scripts/sandbox-change-scope.sh|.github/workflows/sandbox-ci.yml|\
    crates/tiangong-sandbox/*|\
    crates/tiangong-plugin-runtime/Cargo.toml|\
    crates/tiangong-plugin-runtime/src/adapter.rs|\
    crates/tiangong-plugin-runtime/src/host_state.rs|\
    crates/tiangong-plugin-runtime/src/loader.rs|\
    crates/tiangong-plugin-runtime/src/registry.rs|\
    crates/tiangong-plugin-runtime/src/sidecar.rs|\
    crates/tiangong-plugin-runtime/src/sidecar/command.rs|\
    crates/tiangong-plugin-runtime/src/sidecar/stdio.rs|\
    crates/tiangong-plugin-runtime/tests/ephemeral_route.rs|\
    crates/tiangong-plugin-sidecar/src/bin/test-stdio-host.rs|\
    crates/tiangong-plugin-sidecar/src/bin/test-stdio-sidecar.rs|\
    crates/tiangong-plugin-sidecar/src/lib.rs|\
    crates/tiangong-plugin-sidecar/src/server.rs|\
    crates/tiangong-plugin-sidecar/src/stdio.rs|\
    crates/tiangong-plugin-sidecar/tests/stdio_e2e.rs|\
    plugins/tiangong-plugin-command/sidecar/*|\
    src-tauri/tauri.conf.json|\
    xtask/Cargo.toml|xtask/src/main.rs)
      sandbox_change=true
      ;;
    *)
      other_change=true
      ;;
  esac
done

if [[ "$sandbox_change" == "true" && "$other_change" == "false" ]]; then
  echo true
else
  echo false
fi
