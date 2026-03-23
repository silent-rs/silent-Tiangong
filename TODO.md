# TODO - 天工全栈平台重构任务清单

> 最后更新：2026-03-23
> 当前主线 RFC：`docs/rfc/0004-full-stack-agent-platform.md`
> 参考：`PLAN.md`、`docs/requirements.md`

---

## Phase 2：Skill 管理 MVP（已完成 ✅）

- [x] Skill 支持安装、启停、卸载、列表、详情
- [x] `/skill` 管理交互对齐 `/mcp`
- [x] 动态 Step 执行闭环
- [x] Skill 锁文件（skills-lock.json / mcp-lock.json）
- [x] MCP 托管映射（卸载 skill 时自动清理无引用的托管 MCP server）
- [x] 事务回滚（安装失败时 RAII 自动回滚已复制文件）
- [x] 审计日志（~/.tiangong/audit.jsonl，记录 skill/mcp 操作）

---

## Phase 3：Workspace 拆分与核心抽离（已完成 ✅）

### A. Workspace 基础结构

- [x] 创建 `crates/` 目录与 workspace Cargo.toml
- [x] 新建 `crates/tiangong-core/Cargo.toml`，迁移核心依赖
- [x] 新建 `crates/tiangong-cli/Cargo.toml`，CLI 依赖
- [x] 新建 `crates/tiangong-entry/Cargo.toml`，命令路由
- [x] 调整主 `Cargo.toml` 为 workspace 根配置
- [x] 创建空骨架 crate：tiangong-server/gateway/connector/media

### B. 核心引擎迁移 (tiangong-core)

- [x] 迁移 `src/core/*` → `crates/tiangong-core/src/`
- [x] 路径替换 `crate::core::` → `crate::`
- [x] 新增 `context/` 上下文管理模块（压缩器 + 组织器）
- [x] 新增 `plugin/` 插件框架（类型 + 注册表 + 生命周期）
- [x] 修复原有编译错误（model.rs 类型导入、lite_model、tracing 宏）
- [x] 修复可见性问题（pub(in crate::app_state) → pub）
- [x] 确保 `tiangong-core` 可独立编译，无 UI 依赖

### C. CLI 前端 (tiangong-cli)

- [x] Codex 风格 REPL（对话自然滚动，终端原生文本选择）
- [x] 交互式输入（crossterm raw mode 行编辑、历史导航）
- [x] `/` 命令补全 + `@` 提及补全（自动弹出竖排候选列表）
- [x] ratatui modal 管理界面（/sessions、/mcp、/skill）
- [x] 命令系统（/new、/model、/config、/cancel、/help）
- [x] 草稿会话（启动不创建空会话，首次发送才记录）

### D. GUI 前端（src-tauri）

- [x] src-tauri 保持独立 Tauri workspace
- [x] 路径更新 `tiangong_core::core::` → `tiangong_core::`
- [x] 编译验证通过

### E. 主入口统一 (tiangong-entry)

- [x] 迁移 `src/entry/*` → `crates/tiangong-entry/src/`
- [x] 路径替换 + 可见性调整
- [x] CLI 命令恢复：`Some(MainCommand::Cli)` → `tiangong_cli::run_cli()`

### F. 验证与交付

- [x] `cargo fmt -- --check` 通过
- [x] `cargo check --workspace` 通过
- [x] `cargo clippy --workspace --all-targets --tests --benches -- -D warnings` 通过
- [x] CLI 功能可用（REPL + 命令 + modal 管理）
- [x] src-tauri 编译通过
- [x] 死代码清理（src/ui/、src/cli/、src/lib.rs、src/core/、src/entry/）

---

## 后续阶段（待展开）

### Phase 4：Server 模式（已完成 ✅）
- [x] 新建 `crates/tiangong-server`
- [x] 实现 REST API（/chat /sessions /mcp /skills /health /shutdown）
- [ ] 实现 WebSocket 流式通信（待 Phase 5 EventBus 后实现）
- [x] API Token 认证（Bearer Token）
- [x] `-d` / `--daemon` 后台运行支持（PID 文件管理）
- [x] `tiangong server stop` 停止后台 Server
- [ ] Server 启动时自动加载已启用 Connector（待 Phase 6 后实现）

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
