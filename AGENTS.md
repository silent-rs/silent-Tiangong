# AGENTS.md - 天工项目 AI 编码代理指南

本文档为 AI 编码代理提供项目上下文和代码规范。

---

## 项目概述

Silent-Tiangong（天工）是基于 Hetu 框架的桌面级 AI 自动化中枢系统。

- **语言**: Rust (edition 2024)
- **框架**: Hetu (https://github.com/silent-rs/silent)
- **许可**: Apache License 2.0

---

## 构建 / Lint / 测试命令

```bash
# 构建
cargo build --release

# 检查
cargo check --workspace

# Lint (Clippy)
cargo clippy --workspace --all-targets --tests --benches -- -D warnings

# 格式化
cargo fmt
cargo fmt -- --check  # 仅检查

# 依赖检查
cargo deny check --disable-fetch

# 测试
cargo nextest run --workspace                    # 运行所有测试
cargo nextest run --workspace <test_name>        # 运行单个测试
cargo nextest run --workspace --no-tests pass    # 无测试时通过

# 完整检查链（提交前）
cargo fmt -- --check && cargo check --workspace && cargo clippy --workspace --all-targets --tests --benches -- -D warnings && cargo nextest run --workspace --no-tests pass
```

---

## 项目结构

```
tiangong/
├── src/
│   ├── main.rs              # 入口点
│   ├── core/                # 核心业务逻辑
│   │   ├── app_state.rs     # 应用状态管理
│   │   ├── model.rs         # 模型客户端
│   │   ├── planner.rs       # 任务规划
│   │   ├── runtime.rs       # 运行时引擎
│   │   ├── session.rs       # 会话管理
│   │   └── tool.rs          # 工具执行
│   └── ui/                  # UI 层
│       ├── app.rs           # 主应用组件
│       ├── style.css        # 样式
│       └── components/      # UI 组件
├── docs/                    # 文档
├── .tiangong/               # 应用数据（运行时生成）
├── Cargo.toml
├── deny.toml                # 依赖审计配置
└── .pre-commit-config.yaml  # Git hooks
```

---

## 代码风格指南

### 导入顺序

```rust
// 1. 标准库
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// 2. 外部库
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

// 3. 内部模块
use crate::core::model::ModelProviderConfig;
use crate::core::session::Session;
```

### 命名约定

| 类型 | 风格 | 示例 |
|------|------|------|
| 结构体/枚举 | PascalCase | `TiangongState`, `RunStatus` |
| 函数/方法 | snake_case | `load_or_default()`, `send_current_input()` |
| 常量 | SCREAMING_SNAKE_CASE | `DEFAULT_SESSION_TITLE` |
| 模块 | snake_case | `app_state`, `runtime` |
| 字段 | snake_case | `active_session_id` |

### 类型定义

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Idle,
    Planning,
    Executing,
    Completed,
    Failed,
}
```

### 错误处理

```rust
fn load_session(&self, id: &str) -> Result<Session> {
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取会话文件失败：{}", path.display()))?;
    let session: Session = serde_json::from_str(&content)
        .with_context(|| format!("解析会话文件失败：{}", path.display()))?;
    Ok(session)
}
```

### ID 与时间

```rust
fn new_id() -> String {
    scru128::new().to_string()  // 必须使用 scru128，不用 UUID
}

pub fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()  // 使用本地时间
}
```

---

## 开发流程

1. 检查 `PLAN.md` 了解项目方向
2. 查看 `TODO.md` 确定当前任务
3. 为每个任务创建独立分支 (`feature/xxx`)
4. 运行完整检查链确保代码质量
5. 遵循 Git 提交规范

---

## 关键依赖

| 依赖 | 用途 |
|------|------|
| `anyhow` | 错误处理 |
| `serde` / `serde_json` | 序列化 |
| `chrono` | 时间处理 |
| `scru128` | ID 生成 |
| `hetu` | UI 框架 |
| `async-openai` | OpenAI API 客户端 |
| `tokio` | 异步运行时 |

---

## 环境变量

| 变量 | 说明 |
|------|------|
| `API_AUTH_TOKEN` | API 认证令牌 |
| `API_BASE_URL` | API 基础 URL |
| `API_TIMEOUT_MS` | 请求超时（毫秒） |
| `API_MODEL` | 模型名称 |
| `API_STREAM` | 是否启用流式输出（默认 true） |
| `API_CLI_COMMAND` | CLI 模式命令 |
