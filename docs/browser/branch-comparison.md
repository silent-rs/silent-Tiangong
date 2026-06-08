# 分支对比分析报告

> 生成时间：2026-06-07
> 当前分支：`feature/browser-observer-rfc0012`（1 commit，基于 `ff847b7` v0.7.0 后）
> 原开发分支：`feature/smart-element-location`（16 commits，基于 `ff847b7` v0.7.0 后）
> 共同祖先：`ff847b7 fix(browser): 修复内嵌浏览器多项异常 (#119)`

---

## 1. 概览

| 维度 | 当前分支 | 原开发分支 |
|------|---------|-----------|
| 提交数 | 1 | 16 |
| 改动文件数 | 9 | 45 |
| 新增行数 | +1,319 | +12,986 |
| 删除行数 | -51 | -21,192 |
| bridge.js 行数 | 1,607 | 1,827 |
| 核心改动范围 | bridge.js + handler + page_fetcher + types | 全栈（core / plugin / src-tauri / frontend） |

**一句话总结**：当前分支是原开发分支的**精简重做**——只保留了智能元素定位的核心功能，丢弃了原分支中的持久观测层、网络拦截、事件推送、Agent 反馈注入等全部上层能力。

---

## 2. 当前分支包含的内容

当前分支只有 1 个 commit `1454601 feat: add smart browser element locating`，包含：

| 能力 | 文件 | 说明 |
|------|------|------|
| 智能元素定位 | bridge.js | `locateElement` / `locateAll` / `generateSelector` |
| 操作前后对比 | bridge.js | `getPageDigest` / `diffDigest` / `_textDiff` |
| 条件等待 | bridge.js | `waitFor` + 临时 MutationObserver |
| DOM 查询 | bridge.js | `queryDom` |
| 增强点击 | bridge.js | `clickElement` 带 disabled 检测、父级按钮查找 |
| 增强填写 | bridge.js | `fillField` 三层策略（insertText / native / paste） |
| UI 库组件填写 | bridge.js | `fillComponent`（Ant Design / Element Plus） |
| 框架检测 | bridge.js | `detectFramework` |
| 批注元素提取 | bridge.js | `annotation.extractAnnotatedElements` |
| 操作结果差异 | types.rs / handler.rs | `page_diff` 字段、`wait_for_content_change` |
| web_query_dom 工具 | page_fetcher.rs | CSS 选择器查询 DOM |
| Agent prompt 更新 | execution_prompt_agent.rs | 元素定位、操作确认等规则 |

---

## 3. 原开发分支包含但当前分支缺失的内容

### 3.1 持久观测层（observer 模块）❌ 缺失

**原分支实现**（`bridge.js` +220 行）：

- `observer` 对象：持久 MutationObserver + 用户行为监听 + 事件队列
- `_eventQueue` / `_networkQueue` 双队列
- `drainAllEvents()` 接口供 Rust 侧读取
- 500ms 防抖聚合
- 用户点击、输入、导航事件捕获

**当前分支**：完全没有 observer 模块。

### 3.2 网络请求拦截与观测 ❌ 缺失

**原分支实现**（`bridge.js` + `handler.rs` + `types.rs`）：

- 扩展 fetch/XHR 拦截，记录 JSON 响应体
- `NetworkResponse` 事件类型
- 过滤噪音请求

**当前分支**：bridge.js 的 fetch/XHR 拦截仅用于屏蔽 IPC。

### 3.3 事件消费线程 ❌ 缺失

**原分支实现**（`manager.rs` +100 行）：

- `start_event_poll` 独立线程，1 秒间隔读取事件队列
- `BrowserState.pending_events` 缓存
- `ack_events` 确认机制

**当前分支**：没有事件消费线程。

### 3.4 Agent 反馈注入链路 ❌ 缺失

**原分支实现**（跨 7 个文件，+400 行）：

- `Command::InjectBrowserContent` 新增 `feedback` 字段
- `react/message.rs` 区分 `[浏览器反馈]` / `[浏览器内容变化]` / `[浏览器页面更新]` 三种标签
- `src-tauri/src/main.rs` 监听 `browser:events` 并注入

**当前分支**：没有 feedback 机制。

### 3.5 内容签名轮询 ❌ 缺失

**原分支实现**（`manager.rs` +50 行）：

- URL 轮询线程中每 3 秒检测 `innerText` 前 500 字符变化

**当前分支**：URL 轮询线程只检测 URL 变化。

### 3.6 前端测试 ❌ 缺失

**原分支实现**（`frontend/src/__tests__/bridge.test.ts`，1352 行）：

- bridge.js 完整单元测试套件 + CI 集成

**当前分支**：没有前端测试。

---

## 4. 代码质量对比

### 4.1 原分支的问题

| 问题 | 详情 |
|------|------|
| **BrowserEvent 类型重复定义** | `types.rs` 和 `browser_trait.rs` 各定义了一份 |
| **commit 粒度不均** | 16 个 commit 中有 8 个 fix |
| **网络拦截与 IPC 屏蔽耦合** | fetch/XHR 拦截同时承担两个职责 |
| **事件消费与 URL 轮询职责重叠** | 两个线程都在检测页面变化 |
| **feedback 注入链路过长** | 跨越 7 个文件 |

### 4.2 当前分支的问题

| 问题 | 详情 |
|------|------|
| **功能不完整** | 缺少持久观测层、事件推送、Agent 反馈注入 |
| **没有前端测试** | 原分支的 1352 行测试全部丢失 |
| **没有 Agent prompt 更新** | 原分支的规则更新未包含 |

---

## 5. 建议

### 5.1 需要从原分支拣选的内容

| 优先级 | 内容 | 原分支 commit | 理由 |
|:---:|------|-------------|------|
| **P0** | 前端测试套件 | `a5d4ef3` | 1352 行测试，保障 bridge.js 质量 |
| **P0** | Agent prompt 规则更新 | `ecbe70f` + `5cd5b54` | Agent 行为规则 |
| **P1** | 持久观测层 | `16d3bc3` + `984e58e` | 核心能力，需重构 |
| **P1** | 事件消费线程 | `16d3bc3` | observer 的 Rust 侧消费 |
| **P1** | 内容签名轮询 | `65b5008` + `6858d12` | 检测用户手动操作 |
| **P2** | Agent 反馈注入链路 | `984e58e` + `8061c07` | 需简化链路 |

### 5.2 建议不拣选的内容

| 内容 | 理由 |
|------|------|
| **网络请求拦截观测** | 与 IPC 屏蔽耦合，噪音大收益低 |
| **BrowserEvent 重复定义** | 需重新设计，避免重复 |

### 5.3 推荐方案

**在当前分支上增量开发**：先拣选前端测试和 Agent prompt，再按 `03-observer-design.md` 重新实现观测层（参考原分支但不照搬代码）。
