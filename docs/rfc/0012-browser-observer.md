# RFC 0012：嵌入式浏览器实时观测层

> 状态：草案
> 创建：2026-06-07
> 分支：基于 `feature/smart-element-location` 演进

---

## 1. 背景与动机

### 1.1 现状

天工的嵌入式浏览器（Phase 21）已具备完整的操作能力：页面导航、表单提取/填写、元素点击、DOM 查询、批注模式。当前分支 `feature/smart-element-location` 进一步增强了智能元素定位、操作前后对比（`getPageDigest` / `diffDigest`）、条件等待（`waitFor`）和内容变化检测。

但所有"感知浏览器变化"的机制都是**拉取/轮询模式**：

| 现有机制 | 触发方式 | 粒度 |
|---------|---------|------|
| `on_page_load` 回调 | 页面加载完成 | 整页 |
| URL 轮询线程 | 500ms 间隔 | URL 变化 |
| 内容签名轮询 | 3 秒间隔，前 500 字符 | 粗糙 |
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
| **fetch/XHR 拦截** | 浏览器原生 API | ✅ | bridge.js 已有拦截基础 |

**结论**：所有主流外部方案都依赖 CDP（Chrome DevTools Protocol），与 Tauri WebView 架构不兼容。最佳路径是在 bridge.js 中利用浏览器原生 API 构建观测能力。

### 2.2 可借鉴的设计思路

虽然外部方案不能直接使用，但以下设计理念值得参考：

- **playwright-observer**：事件分类（DOM / 网络）、噪音过滤（过滤分析/字体/图片请求）、结构化 JSON 输出
- **browser-use**：DOM 状态提取、可交互元素识别、截图 + DOM 双通道感知
- **Stagehand**：`act()` / `extract()` / `agent()` 三层 API、自愈选择器、操作缓存

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
│  → emit("browser:dom_event", payload)        │
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

观测层产生的语义事件分为三类：

#### 3.2.1 DOM 变化事件（由 MutationObserver 驱动）

| 事件类型 | 触发条件 | 优先级 |
|---------|---------|:---:|
| `dialog_opened` | 检测到 `[role="dialog"]`、`.ant-modal-wrap`、`.el-dialog__wrapper` 等出现 | 高 |
| `dialog_closed` | 上述元素消失 | 高 |
| `loading_started` | 检测到 `aria-busy="true"`、`.ant-spin`、`.el-loading` 等出现 | 中 |
| `loading_finished` | 上述元素消失 | 中 |
| `content_changed` | 主要内容区域（`<main>`、`<article>`、`#app`）的子节点变化 | 中 |
| `button_state_changed` | 按钮的 `disabled` 属性或 CSS 类变化 | 低 |
| `form_changed` | 表单内字段值变化（非用户输入引起的） | 低 |

#### 3.2.2 用户行为事件（由事件监听驱动）

| 事件类型 | 触发条件 | 优先级 |
|---------|---------|:---:|
| `user_click` | 用户点击可交互元素（按钮、链接、输入框） | 高 |
| `user_input` | 用户在输入框中输入内容（1 秒防抖） | 中 |
| `user_scroll` | 用户滚动页面（2 秒防抖） | 低 |
| `user_navigation` | 用户通过页面内链接导航（`popstate` / `hashchange`） | 高 |

#### 3.2.3 ~~网络事件~~（不实现）

> **决策**：网络请求观测（fetch/XHR 拦截、SSE、WebSocket）**不纳入本方案**。理由：
> - bridge.js 已有 fetch/XHR 拦截用于屏蔽 IPC，扩展为网络观测会增加复杂度和性能开销
> - 网络事件噪音极大，过滤成本高，收益有限
> - Agent 感知页面变化主要依赖 DOM 变化和用户行为，网络层信息对协作场景帮助不大
> - 如果未来确实需要，可以作为独立阶段补充

### 3.3 bridge.js observer 模块设计

```javascript
// 在 window.__tiangong_bridge 下新增 observer 对象
observer: {
    // 内部状态
    _eventQueue: [],
    _mutationObserver: null,
    _debounceTimer: null,
    _pendingMutations: [],
    _started: false,

    // 启动持久观测
    start: function() {
        if (this._started) return;
        this._started = true;
        this._startMutationObserver();
        this._bindUserEvents();
    },

    // 停止观测
    stop: function() {
        this._started = false;
        if (this._mutationObserver) {
            this._mutationObserver.disconnect();
            this._mutationObserver = null;
        }
        // 解绑用户事件...
    },

    // Rust 侧调用，读取并清空事件队列
    drainEvents: function() {
        var events = this._eventQueue;
        this._eventQueue = [];
        return events;
    },

    // ── MutationObserver ──

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
        var dialogAdded = false;
        var dialogRemoved = false;
        var loadingAdded = false;
        var loadingRemoved = false;
        var contentChanged = false;

        for (var i = 0; i < mutations.length; i++) {
            var m = mutations[i];

            // 属性变化检测
            if (m.type === 'attributes') {
                var attr = m.attributeName;
                var target = m.target;

                // disabled 状态变化
                if (attr === 'disabled' || attr === 'aria-disabled') {
                    // 检查是否是按钮/提交元素
                    if (target.tagName === 'BUTTON' ||
                        target.closest('button, [role="button"]')) {
                        // 记录但不单独生成事件，合并到其他事件中
                    }
                }
                // aria-busy 变化
                if (attr === 'aria-busy') {
                    if (target.getAttribute('aria-busy') === 'true') {
                        loadingAdded = true;
                    } else {
                        loadingRemoved = true;
                    }
                }
                continue;
            }

            // 子节点变化检测
            if (m.type === 'childList') {
                // 新增节点
                for (var j = 0; j < m.addedNodes.length; j++) {
                    var node = m.addedNodes[j];
                    if (node.nodeType !== 1) continue; // 只关注元素节点

                    // 对话框出现
                    if (this._isDialog(node)) {
                        dialogAdded = true;
                    }
                    // 加载指示器出现
                    if (this._isLoadingIndicator(node)) {
                        loadingAdded = true;
                    }
                    // 主要内容变化
                    if (this._isMainContent(node) || this._isMainContent(m.target)) {
                        contentChanged = true;
                    }
                }
                // 移除节点
                for (var k = 0; k < m.removedNodes.length; k++) {
                    var rNode = m.removedNodes[k];
                    if (rNode.nodeType !== 1) continue;

                    if (this._isDialog(rNode)) {
                        dialogRemoved = true;
                    }
                    if (this._isLoadingIndicator(rNode)) {
                        loadingRemoved = true;
                    }
                }
            }
        }

        // 生成语义事件
        if (dialogAdded) {
            events.push({
                type: 'dialog_opened',
                timestamp: Date.now(),
                detail: this._describeActiveDialog()
            });
        }
        if (dialogRemoved) {
            events.push({
                type: 'dialog_closed',
                timestamp: Date.now()
            });
        }
        if (loadingAdded && !loadingRemoved) {
            events.push({
                type: 'loading_started',
                timestamp: Date.now()
            });
        }
        if (loadingRemoved && !loadingAdded) {
            events.push({
                type: 'loading_finished',
                timestamp: Date.now()
            });
        }
        if (contentChanged) {
            events.push({
                type: 'content_changed',
                timestamp: Date.now(),
                detail: this._getContentSummary()
            });
        }

        return events;
    },

    // ── 用户行为监听 ──

    _bindUserEvents: function() {
        var self = this;

        // 用户点击（capture 阶段）
        document.addEventListener('click', function(e) {
            if (!self._started) return;
            var target = e.target;
            // 只关注可交互元素
            var interactive = target.closest(
                'button, a, input, select, textarea, [role="button"], [role="link"], [role="tab"], summary'
            );
            if (!interactive) return;

            var desc = self._describeElement(interactive);
            self._pushEvent({
                type: 'user_click',
                timestamp: Date.now(),
                element: desc.tag,
                text: desc.text,
                selector: desc.selector
            });
        }, true);

        // 用户输入（1 秒防抖）
        var inputTimer = null;
        var inputTarget = null;
        document.addEventListener('input', function(e) {
            if (!self._started) return;
            inputTarget = e.target;
            if (inputTimer) clearTimeout(inputTimer);
            inputTimer = setTimeout(function() {
                if (!inputTarget) return;
                var desc = self._describeElement(inputTarget);
                self._pushEvent({
                    type: 'user_input',
                    timestamp: Date.now(),
                    selector: desc.selector,
                    label: desc.label || desc.placeholder,
                    valueLength: (inputTarget.value || '').length
                });
                inputTarget = null;
            }, 1000);
        }, true);

        // SPA 路由变化
        window.addEventListener('popstate', function() {
            if (!self._started) return;
            self._pushEvent({
                type: 'user_navigation',
                timestamp: Date.now(),
                url: window.location.href
            });
        });
    },

    // ── 辅助方法 ──

    _isDialog: function(el) {
        if (el.nodeType !== 1) return false;
        if (el.getAttribute && el.getAttribute('role') === 'dialog') return true;
        if (el.classList) {
            return el.classList.contains('ant-modal-wrap') ||
                   el.classList.contains('el-dialog__wrapper') ||
                   el.classList.contains('ant-drawer') ||
                   el.classList.contains('el-drawer');
        }
        return false;
    },

    _isLoadingIndicator: function(el) {
        if (el.nodeType !== 1) return false;
        if (el.getAttribute && el.getAttribute('aria-busy') === 'true') return true;
        if (el.classList) {
            return el.classList.contains('ant-spin') ||
                   el.classList.contains('el-loading') ||
                   el.classList.contains('ant-skeleton');
        }
        return false;
    },

    _isMainContent: function(el) {
        if (el.nodeType !== 1) return false;
        var tag = el.tagName;
        if (tag === 'MAIN' || tag === 'ARTICLE') return true;
        if (el.id === 'app' || el.id === 'root' || el.id === '__next') return true;
        return false;
    },

    _describeElement: function(el) {
        var text = (el.textContent || '').trim().substring(0, 100);
        var tag = (el.tagName || '').toLowerCase();
        var selector = '';
        if (el.id) {
            selector = '#' + el.id;
        } else {
            selector = window.__tiangong_bridge.generateSelector(el);
        }
        var label = '';
        var labelEl = el.closest('[class*="form-item"]');
        if (labelEl) {
            var lbl = labelEl.querySelector('[class*="label"]');
            if (lbl) label = (lbl.textContent || '').trim();
        }
        var placeholder = el.placeholder || el.getAttribute('placeholder') || '';
        return { tag: tag, text: text, selector: selector, label: label, placeholder: placeholder };
    },

    _describeActiveDialog: function() {
        var dialog = window.__tiangong_bridge._getActiveDialog();
        if (!dialog) return '';
        return (dialog.textContent || '').trim().substring(0, 300);
    },

    _getContentSummary: function() {
        var text = (document.body.innerText || '').trim();
        return text.length > 200 ? text.substring(0, 200) + '...' : text;
    },

    _pushEvent: function(event) {
        this._eventQueue.push(event);
        // 限制队列长度，防止内存泄漏
        if (this._eventQueue.length > 100) {
            this._eventQueue = this._eventQueue.slice(-50);
        }
    }
}
```

### 3.4 Rust 侧事件消费

#### 3.4.1 事件类型定义（types.rs 新增）

```rust
/// 浏览器语义事件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserEvent {
    /// 对话框出现
    #[serde(rename = "dialog_opened")]
    DialogOpened {
        timestamp: u64,
        #[serde(default)]
        detail: String,
    },
    /// 对话框关闭
    #[serde(rename = "dialog_closed")]
    DialogClosed { timestamp: u64 },
    /// 加载开始
    #[serde(rename = "loading_started")]
    LoadingStarted { timestamp: u64 },
    /// 加载完成
    #[serde(rename = "loading_finished")]
    LoadingFinished { timestamp: u64 },
    /// 内容变化
    #[serde(rename = "content_changed")]
    ContentChanged {
        timestamp: u64,
        #[serde(default)]
        detail: String,
    },
    /// 用户点击
    #[serde(rename = "user_click")]
    UserClick {
        timestamp: u64,
        element: String,
        text: String,
        selector: String,
    },
    /// 用户输入
    #[serde(rename = "user_input")]
    UserInput {
        timestamp: u64,
        selector: String,
        label: String,
        value_length: usize,
    },
    /// 用户导航
    #[serde(rename = "user_navigation")]
    UserNavigation {
        timestamp: u64,
        url: String,
    },
}
```

#### 3.4.2 事件消费线程（manager.rs 新增）

在 `start_url_poll` 旁边新增 `start_event_poll`，复用类似架构：

```rust
/// 启动事件消费线程，定期读取 bridge.js 的事件队列并 emit
fn start_event_poll(&self, app: &AppHandle<Wry>) {
    let state = self.state.clone();
    let app = app.clone();
    let stop = {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.event_poll_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        s.event_poll_stop.clone().unwrap()
    };

    std::thread::Builder::new()
        .name("browser-event-poll".into())
        .spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1000));
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                let mgr = BrowserManager { state: state.clone() };
                if let Some(raw) = mgr.eval_with_result(
                    "(function(){try{return JSON.stringify(window.__tiangong_bridge.observer.drainEvents())}catch(e){return'[]'}})()"
                ) {
                    if raw == "[]" || raw.is_empty() { continue; }
                    if let Ok(events) = serde_json::from_str::<Vec<BrowserEvent>>(&raw) {
                        if !events.is_empty() {
                            let _ = app.emit("browser:events", &events);
                        }
                    }
                }
            }
        })
        .expect("failed to spawn browser event poll thread");
}
```

#### 3.4.3 BrowserState 新增字段

```rust
pub struct BrowserState {
    // ... 现有字段
    /// 事件消费线程停止信号
    pub event_poll_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
}
```

### 3.5 observer 生命周期管理

observer 在页面加载时自动启动，页面关闭时停止：

- **启动时机**：在 `on_page_load` 回调中（`PageLoadEvent::Finished`），调用 `window.__tiangong_bridge.observer.start()`
- **停止时机**：在 `close()` 方法中，通过 `event_poll_stop` 信号停止消费线程
- **页面导航**：`on_page_load` 会在新页面加载完成时重新触发，observer 自动在新页面上启动（因为 `initialization_script` 每次都会注入 bridge.js）

### 3.6 与现有机制的共存关系

| 现有机制 | 处理方式 | 理由 |
|---------|---------|------|
| `getPageDigest` / `diffDigest` | **保留** | 操作前后的精确对比仍有价值，observer 提供实时感知，digest 提供精确 diff |
| `waitFor` + 临时 MutationObserver | **保留** | observer 是被动感知，waitFor 是主动等待，职责不同 |
| `wait_for_content_change` 轮询 | **保留** | 作为操作后的同步等待机制，observer 是异步事件流 |
| URL 轮询线程 | **保留** | URL 变化检测仍有价值，observer 的 `user_navigation` 事件与之互补 |
| 内容签名轮询（3 秒间隔） | **可移除** | observer 的 `content_changed` 事件更及时、更精确，可以替代 |
| `page_diff` 在操作结果中 | **保留** | 操作结果的差异反馈对 Agent 决策很重要 |
| `web_query_dom` | **保留** | 主动查询与被动推送互补 |

---

## 4. 可移除/简化的功能

在引入持久观测层后，以下功能变得冗余或价值降低，建议评估移除：

### 4.1 建议移除

| 功能 | 位置 | 理由 |
|------|------|------|
| **内容签名轮询**（3 秒间隔，前 500 字符） | `manager.rs` `start_url_poll` 中 `tick.is_multiple_of(6)` 分支 | observer 的 `content_changed` 事件更及时、更精确，完全覆盖此功能 |
| **`_waitInitialState` 中的 `lastMutationTime` 追踪** | `bridge.js` `waitFor` | 持久 observer 已持续追踪 DOM 变化，waitFor 可以直接读取 observer 的状态 |

### 4.2 建议简化

| 功能 | 位置 | 简化方式 |
|------|------|---------|
| **`wait_for_content_change` 中的 innerText 签名轮询** | `handler.rs` | observer 生效后，可以改为读取 observer 事件队列判断变化，减少 eval 调用次数 |
| **`_startMutationObserver` / `_stopMutationObserver`** | `bridge.js` `waitFor` | 持久 observer 运行后，waitFor 不再需要自己管理 Observer，直接复用 |

### 4.3 不建议移除

| 功能 | 理由 |
|------|------|
| URL 轮询线程 | observer 的 `user_navigation` 只能捕获 `popstate`/`hashchange`，无法捕获所有 URL 变化（如 `pushState` 不触发 `popstate`） |
| `getPageDigest` / `diffDigest` | 操作前后精确对比，observer 无法替代 |
| `web_query_dom` | 主动查询能力，与被动推送互补 |

---

## 5. 实施计划

### Phase 22-A：bridge.js observer 模块（P0）

**目标**：在 bridge.js 中实现持久 MutationObserver + 用户行为监听 + 事件队列。

**改动范围**：
- `crates/tiangong-plugin-browser/js/bridge.js`：新增 `observer` 对象
- `crates/tiangong-plugin-browser/src/manager.rs`：`on_page_load` 中启动 observer

**验收标准**：
- observer 在页面加载后自动启动
- MutationObserver 持续运行，500ms 防抖聚合
- 用户点击、输入事件被捕获并入队
- `drainEvents()` 返回结构化 JSON 事件数组
- 队列长度限制在 100 条以内

### Phase 22-B：Rust 侧事件消费（P0）

**目标**：新增事件消费线程，定期读取事件队列并 emit 到前端。

**改动范围**：
- `crates/tiangong-plugin-browser/src/types.rs`：新增 `BrowserEvent` 枚举
- `crates/tiangong-plugin-browser/src/manager.rs`：新增 `start_event_poll`、`BrowserState` 新增字段
- `crates/tiangong-plugin-browser/src/handler.rs`：`close` 时停止事件消费线程

**验收标准**：
- 事件消费线程 1 秒间隔运行
- 事件通过 `app.emit("browser:events", ...)` 推送
- 浏览器关闭时线程正确退出

### Phase 22-C：语义事件聚合优化（P1）

**目标**：优化 MutationObserver 的分析逻辑，生成更精确的语义事件。

**改动范围**：
- `bridge.js` `_analyzeMutations` 方法：完善对话框、加载状态、内容变化的检测规则
- 新增 `_isDialog` / `_isLoadingIndicator` / `_isMainContent` 等辅助方法

**验收标准**：
- 对话框出现/消失被正确检测
- 加载指示器（`aria-busy`、`.ant-spin`、`.el-loading`）被正确检测
- 主要内容区域变化被检测
- 噪音事件（微小样式变化、动画帧）被有效过滤

### Phase 22-D：清理冗余机制（P1）

**目标**：移除被 observer 替代的冗余轮询逻辑。

**改动范围**：
- `manager.rs`：移除 `start_url_poll` 中的内容签名轮询分支
- `bridge.js`：简化 `waitFor` 中的临时 MutationObserver 管理

**验收标准**：
- 移除后功能无回退
- observer 事件流覆盖原有内容变化检测能力

### Phase 22-E：Agent 上下文注入（P2）

**目标**：浏览器事件可选注入 Agent 对话链，使 Agent 在协作中自动感知变化。

**改动范围**：
- `crates/tiangong-core/src/browser_trait.rs`：新增事件订阅接口
- `crates/tiangong-plugin-browser/src/page_fetcher.rs`：实现事件转发
- `crates/tiangong-core/src/agents/`：事件注入对话链的逻辑

**验收标准**：
- Agent 在对话过程中能收到浏览器事件（作为 tool 消息）
- 事件注入受预算控制，不影响正常对话
- 用户可以选择开启/关闭事件注入

### ~~Phase 22-F：前端实时状态展示（P3）~~

> **决策**：前端 BrowserPanel 的实时状态展示（活动时间线、事件日志）**暂不实现**。理由：
> - 前端展示是锦上添花，核心价值在 Agent 侧的感知能力
> - 增加前端复杂度，当前 BrowserPanel 已有基本的状态展示（URL、标签）
> - 后续根据实际使用反馈决定是否需要

### ~~Phase 22-G：网络请求观测~~

> **决策**：网络请求观测（fetch/XHR 拦截扩展、SSE、WebSocket 监控）**不实现**。理由：
> - bridge.js 已有 fetch/XHR 拦截用于屏蔽 IPC，扩展为网络观测会增加复杂度
> - 网络事件噪音极大，过滤成本高
> - Agent 感知页面变化主要依赖 DOM 变化和用户行为，网络层信息对协作场景帮助不大

---

## 6. 风险与缓解

| 风险 | 缓解措施 |
|------|---------|
| MutationObserver 性能开销 | 限制监听属性（`attributeFilter`）、500ms 防抖、队列长度上限 |
| 用户行为事件泄露敏感信息 | `user_input` 事件只记录 `valueLength`，不记录实际值 |
| 事件消费线程与 URL 轮询线程竞争 | 两个线程操作不同的数据，无共享状态竞争 |
| observer 在 SPA 页面导航后失效 | `initialization_script` 每次页面加载都会重新注入 bridge.js，observer 自动重启 |
| 事件队列积压 | 队列长度限制 100 条，超出后丢弃最旧的事件 |

---

## 7. 与需求文档的关系

本方案对应 `docs/requirements.md` 中 Phase 21（内嵌浏览器面板）的后续演进，不引入新的需求条目。核心目标是在现有浏览器能力基础上增强"人机协作"体验，使 Agent 从"被动操作"升级为"主动感知"。

如需将本方案纳入需求文档，建议在 `requirements.md` 的"Should"部分新增：

> - 嵌入式浏览器应支持持久 DOM 变化观测，通过 MutationObserver 和用户行为监听生成语义事件流，供 Agent 实时感知浏览器状态变化。
> - 浏览器观测层应支持事件防抖、队列管理和噪音过滤，避免对页面性能产生明显影响。
