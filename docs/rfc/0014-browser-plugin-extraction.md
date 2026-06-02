# RFC 0014：浏览器能力插件化

> 状态：草稿
> 日期：2026-06-03
> 关联：`tiangong-core`、`src-tauri`、`frontend`、Issue #95

---

## 1. 背景

Phase 21 实现了内嵌浏览器面板，当前代码分布在多个层级：

| 位置 | 内容 |
|------|------|
| `tiangong-types` | `BrowserCommand`（含 `oneshot::Sender`）、`BrowserResponse`、`BrowserPageSnapshot`、`PageStatus` |
| `tiangong-core` | `RuntimeEngine.browser_tx`、`try_browser_fetch()`、`try_browser_observe()`、`set_browser_channel()` |
| `src-tauri/browser.rs` | `BrowserManager`、`browser_command_handler`、JS Bridge |
| `src-tauri/app.rs` | `TiangongApp.browser`、`browser_cmd_tx/rx` |
| `src-tauri/commands.rs` | `browser_open/close/hide/navigate/eval/go_back/go_forward` |
| `frontend/BrowserPanel.tsx` | 浏览器面板 UI 组件 |
| `frontend/MainApp.tsx` | 窗口管理、布局自适应 |

存在以下问题：

1. **`tiangong-types` 引入 `tokio` 依赖**：`BrowserCommand` 包含 `oneshot::Sender`，破坏了 types crate "纯数据结构"的定位。
2. **Core 层直接依赖具体实现类型**：`RuntimeEngine` 持有 `mpsc::Sender<BrowserCommand>`，与 browser 实现耦合。
3. **不可复用**：浏览器能力与天工主应用绑定，无法独立开发、测试或供其他 Tauri 项目使用。
4. **职责混杂**：`src-tauri/browser.rs`（573 行）与 `src-tauri/app.rs`、`src-tauri/commands.rs` 交织，维护成本高。

---

## 2. 目标

- 将浏览器能力封装为独立的 Tauri Plugin crate（`tiangong-plugin-browser`）。
- Core 层通过 trait 抽象与浏览器解耦，不依赖 plugin 的具体类型。
- Plugin 可独立编译、测试，未来可发布到 crates.io。
- 前端布局逻辑保留在应用层，Plugin 只提供能力不提供布局策略。
- 功能不变，纯架构重组。

---

## 3. 目标架构

```
┌──────────────────────────────────────────────────────────────┐
│  Frontend (应用层)                                            │
│  BrowserPanel.tsx ← UI 组件、地址栏、导航按钮                  │
│  MainApp.tsx ← 窗口管理、布局自适应（不迁移到 plugin）          │
│  @tiangong/plugin-browser ← 前端 API（invoke 封装）            │
├──────────────────────────────────────────────────────────────┤
│  tiangong-plugin-browser (Tauri Plugin)                       │
│  src/lib.rs ← plugin::Builder 注册                            │
│  src/manager.rs ← BrowserManager、WebView 生命周期             │
│  src/commands.rs ← browser_open/close/hide/navigate/...       │
│  src/bridge.rs ← JS Bridge Script                             │
│  src/handler.rs ← 命令处理循环                                 │
│  src/page_fetcher.rs ← impl PageFetcher for core              │
│  guest-js/index.ts ← 前端 invoke 封装                         │
├──────────────────────────────────────────────────────────────┤
│  tiangong-core (不变)                                         │
│  src/browser_trait.rs ← PageFetcher trait 定义（新增）         │
│  RuntimeEngine                                                │
│    ├── set_page_fetcher(Arc<dyn PageFetcher>)                 │
│    ├── register_tool_override(name, handler)                  │
│    └── try_browser_fetch() / try_browser_observe()            │
│        → 调用 trait object，不再依赖具体类型                    │
├──────────────────────────────────────────────────────────────┤
│  src-tauri (应用层，精简)                                      │
│  main.rs → .plugin(tiangong_plugin_browser::init())           │
│  app.rs → setup 时注入 PageFetcher 到 core                    │
│  commands.rs → 移除 browser_* commands                        │
│  browser.rs → 删除（迁移到 plugin）                            │
└──────────────────────────────────────────────────────────────┘
```

---

## 4. Core 层 Trait 抽象

### 4.1 PageFetcher Trait

```rust
// tiangong-core/src/browser_trait.rs

/// 浏览器页面获取能力抽象。
///
/// GUI 模式下由 tiangong-plugin-browser 实现，CLI/Server 模式下为 None（回退到 HTTP web_fetch）。
pub trait PageFetcher: Send + Sync + 'static {
    /// 获取指定 URL 的页面内容。
    /// 返回 None 表示能力不可用，调用方应回退到 HTTP 获取。
    fn fetch_page(
        &self,
        url: &str,
        max_chars: usize,
    ) -> Pin<Box<dyn Future<Output = Option<FetchResult>> + Send>>;

    /// 获取当前浏览器页面的快照。
    /// 返回 None 表示浏览器未打开或能力不可用。
    fn observe_page(
        &self,
    ) -> Pin<Box<dyn Future<Output = Option<PageSnapshot>> + Send>>;
}

/// 页面获取结果（纯数据，无 tokio 依赖）
pub struct FetchResult {
    pub ok: bool,
    pub title: String,
    pub content: String,
    pub final_url: String,
    pub error: Option<String>,
}

/// 页面快照（纯数据，无 tokio 依赖）
pub struct PageSnapshot {
    pub title: String,
    pub url: String,
    pub text: String,
}
```

### 4.2 RuntimeEngine 改造

```rust
// tiangong-core/src/runtime.rs

pub struct RuntimeEngine {
    // ... 现有字段 ...
    /// 浏览器页面获取能力（GUI 模式下注入）
    page_fetcher: Option<Arc<dyn PageFetcher>>,
}

impl RuntimeEngine {
    /// 注入页面获取能力（GUI 模式下由 Tauri Plugin 提供）
    pub fn set_page_fetcher(&self, fetcher: Arc<dyn PageFetcher>) {
        // 通过 Command 或直接设置
    }
}
```

### 4.3 工具拦截注册

```rust
// tiangong-core/src/runtime.rs

/// 注册工具覆盖处理器。
/// 当 Agent 调用指定工具时，优先使用注册的处理器。
pub fn register_tool_override(
    &self,
    tool_name: &str,
    handler: Arc<dyn ToolOverrideHandler>,
) { ... }
```

当前硬编码的 `if call.name == "web_fetch"` 拦截改为通过注册机制实现，Plugin 在注入 `PageFetcher` 时同时注册 `web_fetch` 和 `web_browse` 的工具覆盖。

---

## 5. Plugin 结构

### 5.1 Crate 结构

```
tiangong-plugin-browser/
├── Cargo.toml
├── src/
│   ├── lib.rs              ← plugin 入口、Builder 注册
│   ├── manager.rs          ← BrowserManager（从 src-tauri/browser.rs 迁移）
│   ├── commands.rs         ← Tauri commands（从 src-tauri/commands.rs 迁移）
│   ├── bridge.rs           ← JS Bridge Script（从 browser.rs 提取）
│   ├── handler.rs          ← 命令处理循环（从 browser_command_handler 迁移）
│   ├── page_fetcher.rs     ← impl PageFetcher for BrowserPageFetcher
│   └── error.rs            ← 统一错误类型
├── guest-js/
│   ├── index.ts            ← 前端 invoke 封装
│   └── package.json
└── README.md
```

### 5.2 Plugin 入口

```rust
// tiangong-plugin-browser/src/lib.rs

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

pub struct BrowserState {
    manager: BrowserManager,
    cmd_tx: tokio::sync::mpsc::Sender<BrowserCommand>,
}

#[tauri::command]
async fn browser_open(...) -> Result<(), String> { ... }

#[tauri::command]
async fn browser_close(...) -> Result<(), String> { ... }

// ... 其他 commands

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("browser")
        .invoke_handler(tauri::generate_handler![
            browser_open,
            browser_close,
            browser_hide,
            browser_set_position,
            browser_navigate,
            browser_eval,
            browser_go_back,
            browser_go_forward,
        ])
        .setup(|app, _api| {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let manager = BrowserManager::new();
            let state = BrowserState { manager, cmd_tx: tx };
            app.manage(state);

            // 启动命令处理循环
            let browser_state = app.state::<BrowserState>();
            // ... spawn handler

            Ok(())
        })
        .build()
}
```

### 5.3 PageFetcher 实现

```rust
// tiangong-plugin-browser/src/page_fetcher.rs

use tiangong_core::browser_trait::{FetchResult, PageFetcher, PageSnapshot};

pub struct BrowserPageFetcher {
    cmd_tx: tokio::sync::mpsc::Sender<BrowserCommand>,
}

impl PageFetcher for BrowserPageFetcher {
    fn fetch_page(
        &self,
        url: &str,
        max_chars: usize,
    ) -> Pin<Box<dyn Future<Output = Option<FetchResult>> + Send>> {
        let tx = self.cmd_tx.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cmd = BrowserCommand::FetchPage { url, max_chars, response_tx };
            tx.send(cmd).await.ok()?;
            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                response_rx,
            ).await.ok()?.ok()?;
            Some(FetchResult {
                ok: resp.ok,
                title: resp.title,
                content: resp.content,
                final_url: resp.final_url,
                error: resp.error,
            })
        })
    }

    fn observe_page(
        &self,
    ) -> Pin<Box<dyn Future<Output = Option<PageSnapshot>> + Send>> {
        // 类似实现
    }
}
```

---

## 6. 应用层集成

### 6.1 main.rs

```rust
// src-tauri/src/main.rs

fn run_gui() {
    tauri::Builder::default()
        .plugin(tiangong_plugin_browser::init())  // ← 替代手动注册
        // ... 其他 plugins
        .setup(|app| {
            let state = app.state::<TiangongApp>();

            // 注入 PageFetcher 到 core
            let browser_state = app.state::<tiangong_plugin_browser::BrowserState>();
            let fetcher = std::sync::Arc::new(
                tiangong_plugin_browser::BrowserPageFetcher::new(browser_state.cmd_tx.clone())
            );
            state.set_page_fetcher(fetcher);

            Ok(())
        })
}
```

### 6.2 app.rs 精简

```rust
// src-tauri/src/app.rs — 移除以下字段
// - pub browser: BrowserManager          ← 迁移到 plugin state
// - browser_cmd_tx: mpsc::Sender         ← 迁移到 plugin state
// - browser_cmd_rx: Mutex<Option<...>>   ← 迁移到 plugin state
// - start_browser_handler()              ← 迁移到 plugin setup
```

### 6.3 commands.rs 精简

```rust
// src-tauri/src/commands.rs — 移除以下函数
// - browser_open
// - browser_close
// - browser_set_position
// - browser_navigate
// - browser_eval
// - browser_hide
// - browser_go_back
// - browser_go_forward
```

---

## 7. 前端集成

### 7.1 Plugin 前端 API

```typescript
// tiangong-plugin-browser/guest-js/index.ts
import { invoke } from '@tauri-apps/api/core';

export async function browserOpen(url: string, x: number, y: number, w: number, h: number) {
  return invoke('plugin:browser|browser_open', { url, x, y, width: w, height: h });
}

export async function browserClose() {
  return invoke('plugin:browser|browser_close');
}

// ... 其他 API
```

### 7.2 BrowserPanel.tsx 调整

```typescript
// 从 plugin 包导入 API，替代 api.browserOpen(...)
import { browserOpen, browserClose, browserNavigate, ... } from '@tiangong/plugin-browser';
```

### 7.3 不迁移的部分

以下逻辑保留在应用层，不迁移到 plugin：

- `MainApp.tsx` 中的窗口 resize 管理、侧边栏联动
- `BrowserPanel.tsx` 组件本身（布局策略是应用层关注点）
- `MessageInput.tsx` 的 compact 模式
- `MessageList.tsx` 的链接点击拦截

---

## 8. tiangong-types 清理

### 移除

- `browser.rs` 整个文件（`BrowserCommand`、`BrowserResponse`、`BrowserPageSnapshot`、`PageStatus`）
- `Cargo.toml` 中的 `tokio` 依赖

### 迁移去向

| 类型 | 迁移到 |
|------|--------|
| `BrowserCommand` | `tiangong-plugin-browser` 内部 |
| `BrowserResponse` | `tiangong-plugin-browser` 内部 |
| `BrowserPageSnapshot` | `tiangong-core::browser_trait::PageSnapshot`（纯数据版） |
| `PageStatus` | `tiangong-plugin-browser` 内部 |

---

## 9. 实施步骤

### Phase 1：Core 层 Trait 抽象（前置，无功能变化）

1. 新增 `tiangong-core/src/browser_trait.rs`，定义 `PageFetcher` trait、`FetchResult`、`PageSnapshot`
2. `RuntimeEngine` 新增 `page_fetcher: Option<Arc<dyn PageFetcher>>` 字段
3. 新增 `set_page_fetcher()` 方法
4. `try_browser_fetch()` / `try_browser_observe()` 改为调用 trait object
5. 新增 `register_tool_override()` 机制，替代硬编码的工具名拦截
6. **验证**：现有功能不变，trait 为 None 时回退到 HTTP web_fetch

### Phase 2：Plugin Crate 搭建

1. 创建 `tiangong-plugin-browser` crate
2. 迁移 `BrowserManager`、JS Bridge、命令处理循环
3. 实现 `PageFetcher` trait
4. 注册为 Tauri Plugin
5. 创建 `guest-js` 前端 API 包
6. **验证**：Plugin 可独立编译，单元测试通过

### Phase 3：应用层切换

1. `src-tauri/Cargo.toml` 添加 plugin 依赖
2. `main.rs` 使用 `.plugin()` 注册
3. `app.rs` 移除 browser 相关字段和方法
4. `commands.rs` 移除 browser_* commands
5. `setup` 中注入 `PageFetcher` 到 core
6. 前端 API 调用改为 plugin 前端包
7. **验证**：全功能回归测试

### Phase 4：清理

1. 删除 `src-tauri/src/browser.rs`
2. 清理 `tiangong-types` 中的 browser 模块和 tokio 依赖
3. 清理 `tiangong-core` 中旧的 channel 相关代码
4. 更新文档

---

## 10. 风险与缓解

| 风险 | 缓解措施 |
|------|----------|
| Tauri Plugin 的 WebView API 限制 | Phase 21 已验证 Tauri 2 多 WebView 可行性，Plugin 中使用相同的 API |
| Core trait 设计不够前瞻 | 先用最小 trait（fetch + observe），后续按需扩展 |
| 引擎重建时 PageFetcher 丢失 | `set_page_fetcher` 通过 Command 传递，与现有 `SetBrowserChannel` 同机制 |
| 前端 invoke 命名空间变化 | Plugin 自动加 `plugin:browser|` 前缀，guest-js 封装屏蔽差异 |
| 回归风险 | 每个 Phase 独立验证，Phase 1 无功能变化可先合并 |

---

## 11. 验收标准

- [ ] `tiangong-types` 无 `tokio` 依赖，无 browser 相关类型
- [ ] `tiangong-core` 不依赖 `tiangong-plugin-browser`（通过 trait 解耦）
- [ ] `src-tauri/src/browser.rs` 已删除
- [ ] `src-tauri/src/commands.rs` 无 `browser_*` 函数
- [ ] `src-tauri/src/app.rs` 无 browser 相关字段
- [ ] CLI / Server 模式下 `web_fetch` 正常回退到 HTTP
- [ ] GUI 模式下浏览器面板功能与重构前一致
- [ ] 引擎重建（配置变更）后浏览器能力不丢失
