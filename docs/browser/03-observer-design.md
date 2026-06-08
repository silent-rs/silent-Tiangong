# 持久观测层设计

> 状态：草案
> 创建：2026-06-07
> 分支：基于 `feature/browser-observer-rfc0012` 演进

---

## 1. 背景与动机

### 1.1 现状

天工的嵌入式浏览器（Phase 21）已具备完整的操作能力：页面导航、表单提取/填写、元素点击、DOM 查询、批注模式。智能元素定位分支进一步增强了定位、操作前后对比（`getPageDigest` / `diffDigest`）、条件等待（`waitFor`）和内容变化检测。

但所有"感知浏览器变化"的机制都是**拉取/轮询模式**：

| 现有机制 | 触发方式 | 粒度 |
|---------|---------|------|
| `on_page_load` 回调 | 页面加载完成 | 整页 |
| URL 轮询线程 | 500ms 间隔 | URL 变化 |
| `wait_for_content_change` | 操作后主动轮询 | innerText 签名 |
| `getPageDigest` / `diffDigest` | 手动调用 | 快照对比 |
| `waitFor` + MutationObserver | 临时启动，用完销毁 | 仅服务于等待条件 |

### 1.2 核心问题

1. **Agent 无法实时感知用户操作**：用户在浏览器中点击、输入、导航，Agent 完全不知道。
2. **没有持久的事件推送**：所有变化检测都是轮询或手动触发，延迟高、易漏检。
3. **MutationObserver 是一次性的**：`waitFor` 里的 Observer 用完即销毁，不能持续监控。
4. **没有语义事件层**：底层 DOM 变化没有翻译成 Agent 能理解的高层语义。

### 1.3 目标

在现有 bridge.js + handler.rs 架构上，构建一个**持久、主动、推送式**的浏览器观测层，使 Agent 能够：

- 实时感知用户在浏览器中的操作
- 自动检测页面状态变化（对话框、加载状态、内容更新）
- 在协作场景中"看到"浏览器正在发生什么

---

## 2. 技术选型分析

### 2.1 外部方案评估

| 方案 | 原理 | 与 Tauri WebView 兼容 | 评估 |
|------|------|:---:|------|
| **playwright-observer** | Playwright + CDP 连接浏览器 | ❌ | 依赖 CDP 端口，Tauri WebView 不暴露 |
| **browser-use** (97k⭐) | Python + Playwright 驱动浏览器 | ❌ | Python 生态，需独立浏览器实例 |
| **Stagehand** (23k⭐) | TypeScript + CDP 引擎 | ❌ | 依赖 CDP，需独立浏览器实例 |
| **MutationObserver** | 浏览器原生 API | ✅ | 零依赖，与 bridge.js 完美契合 |

**结论**：所有主流外部方案都依赖 CDP（Chrome DevTools Protocol），与 Tauri WebView 架构不兼容。最佳路径是在 bridge.js 中利用浏览器原生 API 构建观测能力。

### 2.2 可借鉴的设计思路

- **playwright-observer**：事件分类（DOM / 网络）、噪音过滤、结构化 JSON 输出
- **browser-use**：DOM 状态提取、可交互元素识别
- **Stagehand**：`act()` / `extract()` / `agent()` 三层 API、自愈选择器

---

## 3. 方案设计

### 3.1 总体架构

```
bridge.js（注入到 WebView 的每个页面）
┌─────────────────────────────────────────────┐
│  observer 模块（新增，持久运行）              │
│  ├── MutationObserver（持久）                │
│  │   → 监听 DOM 结构变化、关键属性变化        │
│  │   → 500ms 防抖窗口聚合                    │
│  │                                           │
│  ├── 用户行为监听（capture 阶段）             │
│  │   → click / input / change / scroll       │
│  │   → 转译为语义操作描述                     │
│  │                                           │
│  └── 事件队列 + drain 接口                    │
│      → 语义事件聚合后入队                     │
│      → Rust 侧定期 drain 读取                │
└─────────────────┬───────────────────────────┘
                  │ eval_with_result("...drainEvents()")
                  ▼
manager.rs（Rust 侧）
┌─────────────────────────────────────────────┐
│  事件消费线程（新增，与 URL 轮询线程并行）     │
│  → 1 秒间隔读取事件队列                       │
│  → 解析语义事件                               │
│  → emit("browser:events", payload)           │
│  → 可选：注入 Agent 对话链                    │
└─────────────────┬───────────────────────────┘
                  │ Tauri emit
                  ▼
前端 + Agent
┌─────────────────────────────────────────────┐
│  前端：BrowserPanel 展示浏览器活动状态        │
│  Agent：事件注入对话上下文，感知浏览器变化     │
└─────────────────────────────────────────────┘
```

### 3.2 事件模型

#### 3.2.1 DOM 变化事件（由 MutationObserver 驱动）

| 事件类型 | 触发条件 | 优先级 |
|---------|---------|:---:|
| `dialog_opened` | 检测到 `[role="dialog"]`、`.ant-modal-wrap`、`.el-dialog__wrapper` 等出现 | 高 |
| `dialog_closed` | 上述元素消失 | 高 |
| `loading_started` | 检测到 `aria-busy="true"`、`.ant-spin`、`.el-loading` 等出现 | 中 |
| `loading_finished` | 上述元素消失 | 中 |
| `content_changed` | 主要内容区域（`<main>`、`<article>`、`#app`）的子节点变化 | 中 |
| `button_state_changed` | 按钮的 `disabled` 属性或 CSS 类变化 | 低 |

#### 3.2.2 用户行为事件（由事件监听驱动）

| 事件类型 | 触发条件 | 优先级 |
|---------|---------|:---:|
| `user_click` | 用户点击可交互元素（按钮、链接、输入框） | 高 |
| `user_input` | 用户在输入框中输入内容（1 秒防抖） | 中 |
| `user_scroll` | 用户滚动页面（2 秒防抖） | 低 |
| `user_navigation` | 用户通过页面内链接导航（`popstate` / `hashchange`） | 高 |

#### 3.2.3 网络事件（不实现）

> **决策**：网络请求观测**不纳入本方案**。理由：
> - bridge.js 已有 fetch/XHR 拦截用于屏蔽 IPC，扩展为网络观测会增加复杂度
> - 网络事件噪音极大，过滤成本高，收益有限
> - Agent 感知页面变化主要依赖 DOM 变化和用户行为

### 3.3 bridge.js observer 模块设计

```javascript
// 在 window.__tiangong_bridge 下新增 observer 对象
observer: {
    _eventQueue: [],
    _mutationObserver: null,
    _debounceTimer: null,
    _pendingMutations: [],
    _started: false,

    start: function() {
        if (this._started) return;
        this._started = true;
        this._startMutationObserver();
        this._bindUserEvents();
    },

    stop: function() {
        this._started = false;
        if (this._mutationObserver) {
            this._mutationObserver.disconnect();
            this._mutationObserver = null;
        }
    },

    // Rust 侧调用，读取并清空事件队列
    drainEvents: function() {
        var events = this._eventQueue;
        this._eventQueue = [];
        return events;
    },

    _startMutationObserver: function() {
        var self = this;
        this._mutationObserver = new MutationObserver(function(mutations) {
            self._pendingMutations = self._pendingMutations.concat(mutations);
            if (self._debounceTimer) clearTimeout(self._debounceTimer);
            self._debounceTimer = setTimeout(function() {
                self._flushMutations();
            }, 500);
        });
        this._mutationObserver.observe(document.body, {
            childList: true,
            subtree: true,
            attributes: true,
            attributeFilter: [
                'class', 'style', 'disabled', 'readonly',
                'aria-busy', 'aria-hidden', 'aria-disabled',
                'aria-expanded', 'aria-selected'
            ]
        });
    },

    _flushMutations: function() {
        var mutations = this._pendingMutations;
        this._pendingMutations = [];
        var events = this._analyzeMutations(mutations);
        for (var i = 0; i < events.length; i++) {
            this._pushEvent(events[i]);
        }
    },

    _analyzeMutations: function(mutations) {
        var events = [];
        var dialogAdded = false, dialogRemoved = false;
        var loadingAdded = false, loadingRemoved = false;
        var contentChanged = false;

        for (var i = 0; i < mutations.length; i++) {
            var m = mutations[i];
            if (m.type === 'attributes') {
                if (m.attributeName === 'aria-busy') {
                    if (m.target.getAttribute('aria-busy') === 'true') loadingAdded = true;
                    else loadingRemoved = true;
                }
                continue;
            }
            if (m.type === 'childList') {
                for (var j = 0; j < m.addedNodes.length; j++) {
                    var node = m.addedNodes[j];
                    if (node.nodeType !== 1) continue;
                    if (this._isDialog(node)) dialogAdded = true;
                    if (this._isLoadingIndicator(node)) loadingAdded = true;
                    if (this._isMainContent(node) || this._isMainContent(m.target)) contentChanged = true;
                }
                for (var k = 0; k < m.removedNodes.length; k++) {
                    var rNode = m.removedNodes[k];
                    if (rNode.nodeType !== 1) continue;
                    if (this._isDialog(rNode)) dialogRemoved = true;
                    if (this._isLoadingIndicator(rNode)) loadingRemoved = true;
                }
            }
        }

        if (dialogAdded) events.push({ type: 'dialog_opened', timestamp: Date.now(), detail: this._describeActiveDialog() });
        if (dialogRemoved) events.push({ type: 'dialog_closed', timestamp: Date.now() });
        if (loadingAdded && !loadingRemoved) events.push({ type: 'loading_started', timestamp: Date.now() });
        if (loadingRemoved && !loadingAdded) events.push({ type: 'loading_finished', timestamp: Date.now() });
        if (contentChanged) events.push({ type: 'content_changed', timestamp: Date.now(), detail: this._getContentSummary() });
        return events;
    },

    _bindUserEvents: function() {
        var self = this;
        document.addEventListener('click', function(e) {
            if (!self._started) return;
            var interactive = e.target.closest('button, a, input, select, textarea, [role="button"], [role="link"], [role="tab"], summary');
            if (!interactive) return;
            var desc = self._describeElement(interactive);
            self._pushEvent({ type: 'user_click', timestamp: Date.now(), element: desc.tag, text: desc.text, selector: desc.selector });
        }, true);

        var inputTimer = null, inputTarget = null;
        document.addEventListener('input', function(e) {
            if (!self._started) return;
            inputTarget = e.target;
            if (inputTimer) clearTimeout(inputTimer);
            inputTimer = setTimeout(function() {
                if (!inputTarget) return;
                var desc = self._describeElement(inputTarget);
                self._pushEvent({ type: 'user_input', timestamp: Date.now(), selector: desc.selector, label: desc.label || desc.placeholder, valueLength: (inputTarget.value || '').length });
                inputTarget = null;
            }, 1000);
        }, true);

        window.addEventListener('popstate', function() {
            if (!self._started) return;
            self._pushEvent({ type: 'user_navigation', timestamp: Date.now(), url: window.location.href });
        });
    },

    _isDialog: function(el) { /* 检测 dialog/overlay */ },
    _isLoadingIndicator: function(el) { /* 检测 loading/spin */ },
    _isMainContent: function(el) { /* 检测 main/article/#app */ },
    _describeElement: function(el) { /* 返回 tag/text/selector/label */ },
    _describeActiveDialog: function() { /* 返回当前对话框文本摘要 */ },
    _getContentSummary: function() { /* 返回页面内容摘要 */ },

    _pushEvent: function(event) {
        this._eventQueue.push(event);
        if (this._eventQueue.length > 100) {
            this._eventQueue = this._eventQueue.slice(-50);
        }
    }
}
```

### 3.4 Rust 侧事件消费

#### 3.4.1 事件类型定义（types.rs 新增）

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserEvent {
    #[serde(rename = "dialog_opened")]
    DialogOpened { timestamp: u64, #[serde(default)] detail: String },
    #[serde(rename = "dialog_closed")]
    DialogClosed { timestamp: u64 },
    #[serde(rename = "loading_started")]
    LoadingStarted { timestamp: u64 },
    #[serde(rename = "loading_finished")]
    LoadingFinished { timestamp: u64 },
    #[serde(rename = "content_changed")]
    ContentChanged { timestamp: u64, #[serde(default)] detail: String },
    #[serde(rename = "user_click")]
    UserClick { timestamp: u64, element: String, text: String, selector: String },
    #[serde(rename = "user_input")]
    UserInput { timestamp: u64, selector: String, label: String, value_length: usize },
    #[serde(rename = "user_navigation")]
    UserNavigation { timestamp: u64, url: String },
}
```

#### 3.4.2 事件消费线程（manager.rs 新增）

```rust
fn start_event_poll(&self, app: &AppHandle<Wry>) {
    // 1 秒间隔读取 bridge.js 事件队列
    // 解析为 Vec<BrowserEvent>
    // emit("browser:events", &events)
}
```

### 3.5 observer 生命周期管理

- **启动**：`on_page_load` 回调中调用 `window.__tiangong_bridge.observer.start()`
- **停止**：`close()` 方法中通过 `event_poll_stop` 信号停止消费线程
- **页面导航**：`initialization_script` 每次注入 bridge.js，observer 自动在新页面上启动

### 3.6 与现有机制的共存关系

| 现有机制 | 处理方式 | 理由 |
|---------|---------|------|
| `getPageDigest` / `diffDigest` | **保留** | 操作前后精确对比，observer 无法替代 |
| `waitFor` + 临时 MutationObserver | **保留** | observer 是被动感知，waitFor 是主动等待 |
| `wait_for_content_change` 轮询 | **保留** | 操作后的同步等待机制 |
| URL 轮询线程 | **保留** | URL 变化检测仍有价值 |
| 内容签名轮询（3 秒间隔） | **可移除** | observer 的 `content_changed` 更及时精确 |
| `page_diff` 在操作结果中 | **保留** | 操作结果差异对 Agent 决策很重要 |
| `web_query_dom` | **保留** | 主动查询与被动推送互补 |

---

## 4. 可移除/简化的功能

### 4.1 建议移除

| 功能 | 位置 | 理由 |
|------|------|------|
| **内容签名轮询**（3 秒间隔，前 500 字符） | `manager.rs` URL 轮询线程中 | observer 的 `content_changed` 事件更及时精确 |

### 4.2 建议简化

| 功能 | 位置 | 简化方式 |
|------|------|---------|
| **`wait_for_content_change` 中的 innerText 签名轮询** | `handler.rs` | 可改为读取 observer 事件队列判断变化 |
| **`_startMutationObserver` / `_stopMutationObserver`** | `bridge.js` `waitFor` | 持久 observer 运行后，waitFor 直接复用 |

### 4.3 不建议移除

| 功能 | 理由 |
|------|------|
| URL 轮询线程 | observer 的 `user_navigation` 只能捕获 `popstate`/`hashchange`，无法捕获所有 URL 变化 |
| `getPageDigest` / `diffDigest` | 操作前后精确对比，observer 无法替代 |
| `web_query_dom` | 主动查询能力，与被动推送互补 |

---

## 5. 实施计划

### Phase 22-A：bridge.js observer 模块（P0）

**改动范围**：`bridge.js` 新增 `observer` 对象 + `manager.rs` `on_page_load` 中启动 observer

**验收标准**：
- observer 在页面加载后自动启动
- MutationObserver 持续运行，500ms 防抖聚合
- 用户点击、输入事件被捕获并入队
- `drainEvents()` 返回结构化 JSON 事件数组
- 队列长度限制在 100 条以内

### Phase 22-B：Rust 侧事件消费（P0）

**改动范围**：`types.rs` 新增 `BrowserEvent` + `manager.rs` 新增 `start_event_poll`

**验收标准**：
- 事件消费线程 1 秒间隔运行
- 事件通过 `app.emit("browser:events", ...)` 推送
- 浏览器关闭时线程正确退出

### Phase 22-C：语义事件聚合优化（P1）

**改动范围**：`bridge.js` `_analyzeMutations` 完善检测规则

**验收标准**：
- 对话框出现/消失被正确检测
- 加载指示器被正确检测
- 噪音事件被有效过滤

### Phase 22-D：清理冗余机制（P1）

**改动范围**：`manager.rs` 移除内容签名轮询 + `bridge.js` 简化 `waitFor`

### Phase 22-E：Agent 上下文注入（P2）

**改动范围**：`browser_trait.rs` 新增事件订阅接口 + `page_fetcher.rs` 事件转发 + Agent 对话链注入

**验收标准**：
- Agent 在对话过程中能收到浏览器事件（作为 tool 消息）
- 事件注入受预算控制，不影响正常对话

### 不实现的功能

| 功能 | 理由 |
|------|------|
| 前端 BrowserPanel 实时状态展示 | 锦上添花，核心价值在 Agent 侧 |
| 网络请求观测（fetch/XHR 拦截扩展） | 噪音大、复杂度高、收益有限 |

---

## 6. 风险与缓解

| 风险 | 缓解措施 |
|------|---------|
| MutationObserver 性能开销 | 限制监听属性（`attributeFilter`）、500ms 防抖、队列长度上限 |
| 用户行为事件泄露敏感信息 | `user_input` 事件只记录 `valueLength`，不记录实际值 |
| 事件消费线程与 URL 轮询线程竞争 | 两个线程操作不同的数据，无共享状态竞争 |
| observer 在 SPA 页面导航后失效 | `initialization_script` 每次页面加载都重新注入 bridge.js |
| 事件队列积压 | 队列长度限制 100 条，超出后丢弃最旧的事件 |
