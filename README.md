# 🌌 Silent-Tiangong（天工）

> 灵感源自《天工开物》
> 基于河图框架构建的桌面级 AI 自动化中枢系统

Silent-Tiangong 是一个面向未来的桌面端 AI 自动化引擎。

它不仅是一个聊天工具，而是一个：

- 🧠 自主推理系统
- 🤖 任务规划与执行引擎
- 🔧 自动化执行平台
- ⚙️ 系统级 Agent 编排核心

---

## 🧭 项目愿景

“天工”意为自然之工、自动之工。

Silent-Tiangong 的目标是：

> 构建一个可自主规划、自主执行、自主扩展的桌面智能系统。

它将成为 Silent 生态中的智能中枢。

---

## ✨ 核心能力

- 大模型对话能力
- 任务拆解与规划
- Agent 执行与调度
- 工具调用与插件扩展
- 系统自动化能力
- 安全沙箱执行环境
- 单一远程模型供应商接入

---

## 🏗 系统架构

Silent-Tiangong 采用分层架构设计，核心引擎与河图框架解耦，并通过单一远程模型供应商提供智能能力。

### 分层架构说明

| 层级 | 模块 | 职责说明 |
|------|------|----------|
| 桌面层 | Desktop UI | 提供原生桌面交互界面，支持对话、任务管理、Agent 状态可视化 |
| 核心引擎层 | Planner | 任务拆解与规划，生成 Task Graph |
| 核心引擎层 | Agent Runtime | Agent 生命周期管理与调度执行 |
| 核心引擎层 | Memory Layer | 上下文管理与长期记忆机制 |
| 核心引擎层 | Tool Executor | 工具调用、插件执行、权限控制 |
| 框架基础层 | 河图运行时 | 异步执行、事件驱动、资源调度 |
| 框架基础层 | 服务与路由 | 内部模块通信与服务抽象 |
| 框架基础层 | 插件系统 | 动态扩展能力 |
| 模型层 | Remote Model Provider | 单一远程模型 API 接入层 |

---

### 模型层设计（单一供应商模式）

Silent-Tiangong 采用与 Codex / Claude 类似的单一远程模型供应商模式，系统仅依赖一个模型 API 提供智能能力。

| 模块 | 职责 |
|------|------|
| Model Client | 封装远程模型 API 请求 |
| Provider Config | API Key 管理与安全存储 |
| Request Handler | 推理请求发送与响应解析 |
| Error & Retry | 错误处理与重试机制 |
| Usage Monitor | Token 使用统计与成本监控 |

### 设计原则

- 统一模型接口
- 简化架构复杂度
- 成本可控
- 安全优先
- 便于未来替换供应商

---

## 🔐 安全设计

Silent-Tiangong 采用多层安全机制：

- API Key 本地加密存储
- 工具执行沙箱隔离
- 文件访问权限控制
- 执行日志可审计
- 最小权限原则

安全是系统的第一公民。

---

## 🧠 工作机制

1. 用户输入任务
2. Planner 拆解任务并生成执行计划
3. Agent Runtime 调度执行
4. Tool Executor 调用实际工具
5. Remote Model Provider 提供智能推理
6. 结果反馈至 UI

---

## 🛣 开发路线图

### Phase 1 —— 基础能力

- 单 Agent 对话能力
- 基础任务执行
- 工具调用接口

### Phase 2 —— 任务编排

- Task Graph 支持
- 多步骤自动执行
- 状态管理系统

### Phase 3 —— 系统自动化

- 文件系统操作
- Shell 执行沙箱
- 权限管理系统

### Phase 4 —— 扩展能力

- 插件扩展机制
- 自动化工作流模板
- DevOps 自动化能力

### Phase 5 —— 自主智能

- 反思机制（Reflection）
- 自我优化策略
- 动态执行优化

---

## 🧩 项目定位

Silent-Tiangong 不是简单的 ChatGPT 桌面壳。

它是：

- 一个桌面 AI 自动化核心
- 一个可编程的智能执行系统
- 一个系统级 Agent 调度引擎
- Silent 生态的智能中枢

---

## 🖥 CLI 命令

```bash
# 默认启动桌面 UI
tiangong

# CLI 模式
tiangong cli

# 桌面 UI 模式（RFC 0001 暂停迭代）
tiangong ui
```

---

## 📜 许可证

Apache License 2.0
