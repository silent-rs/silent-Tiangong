# T8 - 端到端验证与清理

## 目标
全工作区编译验证、测试通过、清理遗留兼容代码与过时注释，确保整体交付质量。

## 范围
- 全 workspace（`cargo check --workspace --tests`）
- `crates/tiangong-core/`、`crates/plugins/tiangong-plugin-browser/`、`src-tauri/`
- 文档（PLAN.md / TODO.md）

## 依赖
- 前置任务：T1-T7 全部完成
- 后续任务：无（收尾）
- 可并行任务：无
- 阻塞说明：所有功能任务完成后才能做端到端验证。

## 任务
- `cargo check --workspace --tests` 零 warning/error。
- `cargo test`：core / browser / terminal / app-state 全过。
- `cargo fmt --all --check` 通过。
- 清理：core 文档中对浏览器 session 的过时描述。
- 更新 PLAN.md / TODO.md 记录本次架构演进。
- 检查无遗留死代码（如旧的 BrowserContent 若确认无用则清理——需确认 src-tauri main.rs 仍用则保留）。

## 不做
- 不做性能优化。
- 不废弃 Core Session.tabs（后续单独）。
- 不引入新功能。

## 验收
- 全 workspace 编译零 warning/error。
- 全部测试通过。
- 文档更新。

## 验证
- `cargo check --workspace --tests`
- `cargo test -p tiangong-core --lib`
- `cargo test -p tiangong-plugin-browser --lib`
- `cargo test -p tiangong-plugin-terminal --lib`
- `cargo test -p tiangong-app-state`
- `cargo fmt --all --check`
- **手动验证（必须，macOS）**：见 RFC 0016 验收清单——多 session 切换不丢页面、cookie 隔离、恢复、Agent 路由隔离。
