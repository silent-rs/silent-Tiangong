# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

**Silent-Tiangong（天工）** 是基于 Hetu 框架的桌面级 AI 自动化中枢系统，实现"可对话、可规划、可执行、可扩展、可治理"的 Agent 能力闭环。

- **语言**: Rust (edition 2024)
- **框架**: Hetu (GUI) + ratatui (TUI)
- **许可**: Apache License 2.0
- **存储位置**: `~/.tiangong/`

## 常用开发命令

```bash
# 构建
cargo build --release

# 检查（快速验证）
cargo check --workspace

# Lint（严格遵守 warnings）
cargo clippy --workspace --all-targets --tests --benches -- -D warnings

# 格式化
cargo fmt
cargo fmt -- --check  # 仅检查

# 依赖检查
cargo deny check --disable-fetch

# 测试
cargo nextest run --workspace
cargo nextest run --workspace --no-tests pass  # 无测试时通过

# 完整检查链（提交前自动执行）
cargo fmt -- --check && cargo check --workspace && cargo clippy --workspace --all-targets --tests --benches -- -D warnings && cargo nextest run --workspace --no-tests pass

# 运行应用
cargo run --release              # 桌面 UI 模式（默认）
cargo run --release -- cli       # CLI 模式
```

## 核心架构（5 层设计）

当前 `src/core` 采用分层架构，从内到外分为：

### 1. 配置与状态层（`app_state/`）
应用状态的 façade 层，负责状态切片、持久化、服务协调：

- `store/` - 内存状态容器（Session/Provider/Agent/Runtime/UiState）
- `repository/` - 持久化层（文件读写、锁文件管理）
- `services/` - 业务服务层（Turn/Skill/Mcp 服务）
- `facade/` - 对外统一入口（TiangongState）

### 2. 智能体层（`agents/`）
负责各类 LLM 智能体的推理与决策：

- `planning_agent.rs` - 规划智能体
- `response_agent.rs` - 响应生成智能体
- `skill_convert_agent.rs` - 外部 skill 转换智能体
- `execution_prompt_agent.rs` - 执行阶段 prompt 构造
- `execution_completion_agent.rs` - 步骤完成判定
- `execution_tool_agent.rs` - 本地函数工具定义与转换
- `execution_mcp_agent.rs` - MCP 函数工具暴露与路由

### 3. 执行器层（`execution/`）
承接 plan 执行推进、结果归一化与验证逻辑：

- `plan_runner.rs` - 推进 plan item 和 execution step
- `step_executor.rs` - 单个 step 的多轮执行主控
- `result_analyzer.rs` - 提取成功业务结果、聚合 LLM 输出
- `verify.rs` - 推荐并执行验证命令
- `types.rs` / `message.rs` - 执行器领域共享类型与消息构造

### 4. 能力层
- `tool/` + `tool.rs` - 本地工具能力（文件读写、目录遍历、命令执行、代码搜索等）
- `mcp/` - MCP client、配置、上下文、能力缓存
- `skill/` - Skill 分析、上下文、初始化、打包相关逻辑

### 5. 运行时装配层
- `runtime.rs` - `RuntimeEngine` 对外统一入口，装配 `planning -> execution -> response`
- `model.rs` - 模型客户端抽象
- `agent_config.rs` - 模型/MCP/Skill 配置结构
- `planner.rs` - 计划结构与状态模型
- `session.rs` - 会话数据结构

## 执行流程主链路

```
app_state::TiangongState
  -> RuntimeEngine::execute_turn_with_streaming
    -> planning_agent::build_plan_with_agent_with_trace
    -> execution::execute_plan_with_execution_agent
      -> execution::plan_runner（推进 plan items）
        -> execution::step_executor（多轮执行单个 step）
          -> execution_prompt_agent（构造 prompt）
          -> model::complete_with_functions（LLM 推理）
            -> execution_mcp_agent（MCP 工具路由）或 execution_tool_agent（本地工具）
          -> execution_completion_agent（完成判定）
    -> execution::verify（推荐并执行验证命令）
    -> response_agent::build_grounded_response_prompt（生成最终响应）
```

详细时序图参见：`docs/core-architecture.md`

## 代码风格约定

### 导入顺序
```rust
// 1. 标准库
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// 2. 外部库
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

// 3. 内部模块
use crate::core::model::ModelClient;
use crate::core::session::Session;
```

### 命名约定
| 类型 | 风格 | 示例 |
|------|------|------|
| 结构体/枚举 | PascalCase | `TiangongState`, `RunStatus` |
| 函数/方法 | snake_case | `load_or_default()`, `execute_turn_with_streaming` |
| 常量 | SCREAMING_SNAKE_CASE | `DEFAULT_SESSION_TITLE` |
| 模块 | snake_case | `app_state`, `execution` |

### ID 与时间
```rust
// ID 生成必须使用 scru128，不用 UUID
let id = scru128::new().to_string();

// 时间使用本地时间
let now = chrono::Local::now().naive_local();
```

### 错误处理
```rust
use anyhow::{Context, Result, anyhow};

fn load_session(&self, id: &str) -> Result<Session> {
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取会话文件失败：{}", path.display()))?;
    // ...
    Ok(session)
}
```

## 当前开发阶段（RFC 0003）

**当前增量主线**：`docs/rfc/0003-skill-market.md`

核心目标：交付"与 MCP 一致"的 Skill 生命周期管理与依赖托管能力。

关键文档：
- `PLAN.md` - 项目总体规划和里程碑
- `TODO.md` - 基于 PLAN 的当前阶段任务列表
- `docs/requirements.md` - 需求边界与 Must/Should/非目标
- `docs/core-architecture.md` - 核心架构详细说明
- `docs/app-state-redesign.md` - app_state 重构设计稿

## 关键依赖

| 依赖 | 用途 |
|------|------|
| `anyhow` | 错误处理 |
| `serde` / `serde_json` | 序列化 |
| `chrono` | 时间处理（本地时间） |
| `scru128` | ID 生成（必须使用） |
| `hetu` | UI 框架 |
| `components` (hetu-components) | UI 组件库 |
| `async-openai` | OpenAI API 客户端（fork 版本） |
| `tokio` | 异步运行时 |
| `rmcp` | MCP 客户端 |
| `ratatui` | TUI 框架 |

## 环境变量

| 变量 | 说明 |
|------|------|
| `API_AUTH_TOKEN` | API 认证令牌 |
| `API_BASE_URL` | API 基础 URL |
| `API_TIMEOUT_MS` | 请求超时（毫秒） |
| `API_MODEL` | 模型名称 |
| `API_STREAM` | 是否启用流式输出（默认 true） |
| `API_CLI_COMMAND` | CLI 模式命令 |

## 存储结构

```
~/.tiangong/
  app.json              # 应用主配置（会话/模型/UI 状态）
  skills.json           # Skill 配置
  mcp.json              # MCP 配置
  skills-lock.json      # Skill 依赖锁文件
  mcp-lock.json         # MCP 依赖锁文件
  mcp-tools-cache.json  # MCP 能力缓存
  sessions/             # 会话持久化目录
```

## 常见问题

### RefCell 借用冲突
组件使用 `Rc<RefCell<>>` 包裹状态时，在闭包中访问要先 `borrow()` 提取需要的值，再 `borrow_mut()` 修改：
```rust
let (disabled, readonly) = {
    let s = self.state.borrow();
    (s.disabled, s.readonly)  // 先提取需要的值
};
// 再进行可变借用
let mut s = self.state.borrow_mut();
```

### 执行架构边界
`agents` 仅负责 planning/execution/response 等智能体推理，计划推进、结果归一化、验证执行等执行器逻辑需沉淀到 `execution` 领域模块，为后续基于配置文件的智能体装配与替换提供稳定边界。

### 动态 Step 推进
执行阶段采用动态 step 推进：初始计划仅保留可执行事项（plan item），每个 step 在执行后都需判断是否继续、补充或终止后续 step，避免预置 step 缺失导致误完成。
