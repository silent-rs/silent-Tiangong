# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-03-20
> 当前主线 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## Phase 3：Workspace 拆分与核心抽离

### A. Workspace 基础结构

- [ ] 创建 `crates/` 目录与 workspace Cargo.toml
- [ ] 新建 `crates/tiangong-core/Cargo.toml`，迁移核心依赖
- [ ] 新建 `crates/tiangong-cli/Cargo.toml`，迁移 CLI/TUI 依赖
- [ ] 新建 `crates/tiangong-gui/Cargo.toml`，迁移 GUI 依赖（Tauri）
- [ ] 调整主 `Cargo.toml` 为 workspace 根配置

依赖：无

### B. 核心引擎迁移 (tiangong-core)

- [ ] 迁移 `src/core/app_state/` → `crates/tiangong-core/src/app_state/`
- [ ] 迁移 `src/core/agents/` → `crates/tiangong-core/src/agents/`
- [ ] 迁移 `src/core/execution/` → `crates/tiangong-core/src/execution/`
- [ ] 迁移 `src/core/model.rs` → `crates/tiangong-core/src/model/`
- [ ] 迁移 `src/core/session.rs` → `crates/tiangong-core/src/session/`
- [ ] 迁移 `src/core/tool/` → `crates/tiangong-core/src/tool/`
- [ ] 迁移 `src/core/mcp/` → `crates/tiangong-core/src/mcp/`
- [ ] 迁移 `src/core/skill/` → `crates/tiangong-core/src/skill/`
- [ ] 迁移 `src/core/runtime.rs` → `crates/tiangong-core/src/runtime/`
- [ ] 迁移辅助模块（planner.rs、agent_config.rs 等）
- [ ] 确保 `tiangong-core` 可独立编译，无 UI 依赖

依赖：A

### C. CLI/TUI 前端迁移 (tiangong-cli)

- [ ] 迁移 `src/cli/` → `crates/tiangong-cli/src/`
- [ ] 迁移 `src/entry/` → `crates/tiangong-cli/src/entry/`
- [ ] `tiangong-cli` 依赖 `tiangong-core`
- [ ] 确保 `tiangong cli` 命令正常工作

依赖：B

### D. GUI 前端迁移 (tiangong-gui)

- [ ] 迁移 `src/ui/` → `crates/tiangong-gui/src/`
- [ ] `tiangong-gui` 依赖 `tiangong-core`
- [ ] 确保桌面 GUI 正常启动

依赖：B

### E. 主入口统一

- [ ] 重构 `src/main.rs` 为统一入口，根据子命令分发到各 crate
- [ ] 支持 `tiangong`（GUI）、`tiangong cli`（CLI）、`tiangong server`（预留）
- [ ] 确保 `cargo build --release` 正常构建

依赖：C、D

### F. 验证与交付

- [ ] `cargo fmt -- --check` 通过
- [ ] `cargo check --workspace` 通过
- [ ] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [ ] 现有 CLI 功能回归验证
- [ ] 现有 GUI 功能回归验证

依赖：E

---

## 后续阶段（待 Phase 3 完成后展开）

### Phase 4：Server 模式
- [ ] 新建 `crates/tiangong-server`
- [ ] 实现 REST API（对话、会话管理）
- [ ] 实现 WebSocket 流式通信
- [ ] API Token 认证
- [ ] `-d` / `--daemon` 后台运行支持（PID 文件管理）
- [ ] `tiangong server stop` 停止后台 Server
- [ ] Server 启动时自动加载已启用 Connector

### Phase 5：Gateway 与事件总线
- [ ] 实现 EventBus（tokio broadcast）
- [ ] 实现 Gateway 消息路由
- [ ] 统一消息模型

### Phase 6：Connector 框架
- [ ] 定义 Connector trait
- [ ] 实现 Telegram Connector
- [ ] 实现 Discord Connector
- [ ] 实现飞书/Lark Connector
- [ ] 实现 Webhook Connector

### Phase 7：多媒体能力
- [ ] 新建 `crates/tiangong-media`
- [ ] 图片生成（DALL-E / GPT-Image）
- [ ] 视频生成（Sora / Kling）
- [ ] 语音识别（OpenAI Whisper）
- [ ] 语音合成（OpenAI TTS）
- [ ] MediaAgent 集成
- [ ] Connector 语音消息自动转文字

### Phase 8：生产化与完善
- [ ] Docker 部署支持
- [ ] 安全加固
- [ ] 配置热重载
