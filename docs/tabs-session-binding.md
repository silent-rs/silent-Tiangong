# 统一工作区 Tabs 与会话绑定设计

> 来源参考：`/Users/hubertshelley/Documents/silent/tiangong-tabs-session/docs/tabs-session-binding.md`
>
> 当前分支：`feature/unified-tabs-session-binding`

## 完成标准

本功能完成时必须同时满足：

- 浏览器和终端共用一个工作区面板，同一条 Tab 栏内可以混排浏览器 Tab 与终端 Tab。
- 每个对话会话拥有自己的工作区 Tab 列表，切换会话后恢复该会话的 Tab 集合和活跃 Tab。
- 同一会话内可以打开多个终端 Tab，每个终端 Tab 拥有独立 PTY、cwd、输出历史和日志文件。
- 浏览器 Tab 只在需要真实页面时创建 WebView，`about:blank` 空白 Tab 不提前创建 WebView。
- `run_command` / `run_shell` 保持通过终端执行；执行前先检测当前会话终端状态，优先选择已打开且空闲的终端。
- 当前会话没有可用终端，或所有已打开终端都处于繁忙状态时，系统自动创建新终端执行本次命令。
- 如果本次命令是在自动新建的终端中执行，工具结果必须明确告知 Agent 没有复用旧终端。
- 本地验证至少通过 `cargo fmt -- --check`、`cargo check --workspace`、`yarn build`。

## 设计原则

- **元数据和运行实例分离**：会话文件只保存 Tab 元数据，不保存 PTY、WebView、xterm 实例等运行对象。
- **Session 是持久化边界**：工作区 Tab 归属于对话 Session，切换会话时不得复用旧会话的 Tab 状态。
- **插件各自管理运行时**：Core 只保存会话元数据；终端插件管理 PTY；浏览器插件管理 WebView。
- **前端只协调，不成为事实源**：前端负责展示、触发命令和防抖持久化；真实运行对象以后端插件状态为准。
- **Agent 命令落在可见终端**：Desktop 模式下普通命令执行优先落在当前会话的存活终端里，用户可以看到命令过程。

## 范围

### 本阶段要做

- 新增统一工作区 Tab 数据模型。
- 新增会话 Tab 读取和写入命令。
- 改造终端插件支持多 Tab。
- 改造浏览器插件支持会话切换和 Tab 快照。
- 改造前端主布局为单一工作区面板。
- 新增统一 Tabs 容器，按 Tab 类型渲染浏览器或终端内容。
- 更新 Agent 终端相关执行规则。

### 本阶段不做

- 不持久化 PTY 进程本身。
- 不持久化 WebView 实例、页面内 JS 状态或浏览器进程。
- 不新增独立窗口式工作区。
- 不重写浏览器全局历史能力。
- 不重写终端底层 PTY 协议。

## 数据模型

位置：`crates/tiangong-core/src/session.rs`

```rust
pub tabs: Vec<TabState>,
pub active_tab_id: Option<String>,

#[serde(rename_all = "lowercase")]
pub enum TabKind {
    Browser,
    Terminal,
}

pub struct TabState {
    pub id: String,
    pub kind: TabKind,
    pub title: String,
    pub url: String,
    pub created_at: String,
}
```

字段说明：

- `tabs`：当前会话的工作区 Tab 元数据列表。
- `active_tab_id`：当前会话最后活跃的工作区 Tab。
- `TabState.id`：统一 Tab id。终端 Tab 和浏览器 Tab 均使用 scru128。
- `TabState.kind`：区分浏览器和终端。
- `TabState.url`：浏览器 Tab 使用；终端 Tab 为空。
- `TabState.created_at`：终端 Tab 使用；浏览器 Tab 可为空。

兼容策略：

- 所有新增字段必须使用 `#[serde(default)]`。
- 老会话没有 `tabs` 时视为未打开工作区面板。
- 不做一次性迁移脚本，首次打开面板后由前端创建首个 Tab 并持久化。

## 后端设计

### Core / App 命令

位置：`src-tauri/src/commands.rs`

新增命令：

| 命令 | 作用 |
|------|------|
| `get_session_tabs(session_id)` | 读取会话持久化的统一 Tab 元数据 |
| `set_session_tabs(session_id, tabs, active_tab_id)` | 写入会话 Tab 元数据并持久化 |

要求：

- 只读写 `Session.tabs` 和 `Session.active_tab_id`。
- 反序列化失败时不得破坏原会话数据。
- 不在这些命令中创建 PTY 或 WebView。

### 终端插件

位置：`crates/tiangong-plugin-terminal`

运行时结构：

```text
SessionPtyRegistry
  sessions: HashMap<session_id, Arc<SessionTabs>>

SessionTabs
  tabs: HashMap<tab_id, SessionPty>
  active_tab_id: Option<String>
  activity: TerminalActivityTracker
  activity: TerminalActivityTracker
  active_tab_id: Option<String>

SessionPty
  tab_id
  title
  created_at
  manager
  cmd_tx
```

终端命令：

| 命令 | 作用 |
|------|------|
| `terminal_tab_list(session_id)` | 列出会话内所有终端 Tab |
| `terminal_tab_new(session_id, title?, cwd?)` | 新建终端 Tab |
| `terminal_tab_switch(session_id, tab_id)` | 切换活跃终端 Tab |
| `terminal_tab_close(session_id, tab_id)` | 关闭终端 Tab |
| `terminal_tab_restore(session_id, tab_id, title)` | 根据持久化元数据恢复终端 Tab |

关键规则：

- 对外仍使用 `TerminalProvider` trait，不扩大 Core trait 面。
- 支持复合 id：`session_id:tab_id`。
- `run_command` / `run_shell` 未指定 `tab_id` 时，优先选择当前会话中已打开且空闲的终端。
- 若当前会话没有可用终端，或所有终端都繁忙，则自动创建新终端并用它执行本次命令。
- 工具结果需要携带终端选择信息：复用旧终端 / 新建终端、终端 id、繁忙原因摘要。
- 日志路径使用 `~/.tiangong/sessions/<session_id>/terminal-<tab_id>.log`。
- `terminal_send` 写入当前会话选中的可用终端，供 Agent 操作交互式进程。

### 浏览器插件

位置：`crates/tiangong-plugin-browser`

新增能力：

| 命令 | 作用 |
|------|------|
| `browser_snapshot_tabs()` | 返回当前浏览器 Tab 快照 |
| `browser_switch_session(session_id, tabs_to_restore, active_tab_id)` | 切换浏览器运行时所属会话 |

关键规则：

- `BrowserManager` 记录当前绑定的 `active_session_id`。
- 切换会话时关闭旧会话 WebView，清理页面快照和待消费事件。
- 按新会话的 Tab 元数据恢复浏览器 Tab 状态。
- `about:blank` 只恢复元数据，不提前创建 WebView。
- 首次真实导航时再创建 WebView。

## 前端设计

### MainApp

位置：`frontend/src/pages/MainApp.tsx`

改造目标：

- 删除 `showBrowser` / `showTerminal` 两套面板状态。
- 使用单一 `showWorkspacePanel` 控制工作区面板。
- 状态栏的浏览器按钮和终端按钮都打开同一个面板。
- 面板未打开时，按钮点击决定首个 Tab 类型。
- 面板已打开时，按钮点击新增对应类型的 Tab。

建议状态流：

```text
StatusPanel 点击终端/浏览器
  -> MainApp.openOrAddTab(kind)
    -> 面板未打开：setInitialTabKind(kind) + 打开面板
    -> 面板已打开：调用 TabsContainer 暴露的新增 Tab 入口
```

避免使用全局 `window.__pendingTabKind` 作为长期状态源；如需事件桥接，只作为短期兼容手段。

### TabsContainer

位置：`frontend/src/components/TabsContainer.tsx`

职责：

- 维护当前会话的统一 Tab 列表。
- 渲染顶部统一 Tab 栏。
- 新建、切换、关闭浏览器或终端 Tab。
- 会话切换时从后端加载 `Session.tabs`。
- 根据 Tab 类型调用对应插件恢复运行实例。
- 防抖调用 `set_session_tabs` 持久化元数据。

关键状态：

```ts
type TabKind = 'browser' | 'terminal';

interface TabState {
  id: string;
  kind: TabKind;
  title: string;
  url: string;
  created_at: string;
}
```

关键规则：

- hydrate 期间不得触发空列表持久化覆盖已有会话。
- 关闭最后一个 Tab 时关闭工作区面板。
- 新建 Tab 后立即设为活跃。
- 切换 Tab 时先同步后端活跃 Tab，再更新前端活跃状态。

### TerminalTabContent

位置：`frontend/src/components/TerminalTabContent.tsx`

职责：

- 以 `sessionId:tabId` 调用终端插件命令。
- 挂载 xterm.js。
- 监听 `terminal:output`，只写入匹配复合 id 的终端实例。
- 用户输入仍通过 `terminal_session_send_input` 发送到对应 Tab。
- 后端自动新建终端后，前端应能收到终端状态变化并显示对应 Tab。

建议：

- xterm 实例可以由组件局部维护，也可以由池按复合 id 缓存。
- 如果多个终端内容常驻挂载，后端必须使用打开计数而不是布尔值。

### BrowserTabContent

位置：`frontend/src/components/BrowserTabContent.tsx`

职责：

- 地址栏、导航、刷新、缩放、批注和历史入口。
- 活跃时调用 `browser_tab_switch(tabId)`。
- 活跃时同步 WebView 位置。
- `about:blank` 不调用 `browser_open`，首次真实导航才打开 WebView。

## Agent 行为

### 命令执行

- `run_command` 和 `run_shell` 在 Desktop 模式下使用终端执行。
- 执行前先检查当前会话已有终端的存活状态和繁忙状态。
- 如果存在已打开且空闲的终端，选取第一个空闲终端执行命令。
- 如果没有可用终端，或所有终端都繁忙，自动创建一个新终端后执行命令。
- 如果自动创建了新终端，工具结果必须明确说明本次没有复用旧终端，而是在新终端中执行。
- 保留原有命令白名单、路径边界、环境变量加载、超时和输出截断。

### 终端选择与协作

- Agent 不直接调用“打开新终端”工具；新终端创建由 `run_command` / `run_shell` 执行前的终端选择策略自动决定。
- `terminal_send` 写入当前会话选中的可用终端。
- 对交互式进程仍要求先用 `run_shell{interactive:true}` 启动，再用 `terminal_send` 分步操作。

## 开发拆分

### M-1：文档和需求对齐

- 引入开发文档。
- 更新 `docs/requirements.md`。
- 更新 `TODO.md`，拆分后续任务。

### M-2：会话元数据

- 在 Session 中增加 `tabs` / `active_tab_id`。
- 增加 `get_session_tabs` / `set_session_tabs`。
- 保证老会话可加载。

### M-3：终端多 Tab

- 改造 `SessionPtyRegistry`。
- 增加终端 Tab 命令和权限声明。
- 增加复合 id 路由。
- 增加终端忙闲状态检测。
- 增加命令执行前的空闲终端选择策略。
- 增加所有终端繁忙时自动新建终端执行的策略。
- 在工具结果中反馈本次使用的终端和是否为新建终端。

### M-4：浏览器会话绑定

- 增加浏览器 Tab 快照命令。
- 增加浏览器会话切换命令。
- 修复 `about:blank` 恢复策略。

### M-5：前端统一面板

- 改造 `MainApp` 为单一工作区面板。
- 新增 `TabsContainer`。
- 拆分 `BrowserTabContent` 和 `TerminalTabContent`。
- 改造 `StatusPanel` 的浏览器/终端入口。

### M-6：验证

- 运行 `cargo fmt -- --check`。
- 运行 `cargo check --workspace`。
- 运行 `yarn build`。
- 手动确认新建、切换、关闭、会话切换路径。

## 验收清单

- [ ] 新会话打开终端按钮后出现一个终端 Tab。
- [ ] 同一会话可以新增多个终端 Tab，切换后 cwd 和输出互不覆盖。
- [ ] 新会话打开浏览器按钮后出现一个浏览器 Tab。
- [ ] 浏览器 `about:blank` 不提前创建 WebView，输入 URL 后正常打开页面。
- [ ] 同一个 Tab 栏内可以混排浏览器和终端。
- [ ] 切换对话后恢复各自 Tab 列表和活跃 Tab。
- [ ] `run_command` / `run_shell` 在已有空闲终端时复用该终端执行。
- [ ] 当前会话没有可用终端时，`run_command` / `run_shell` 会先创建终端再执行。
- [ ] 当前会话所有终端繁忙时，`run_command` / `run_shell` 会新建终端执行。
- [ ] 新建终端执行时，工具结果会告知 Agent 本次使用了新终端。
- [ ] `terminal_send` 可以写入当前选中的可用终端。
