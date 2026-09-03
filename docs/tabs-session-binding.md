# 统一工作区 Tabs 与会话绑定设计

> 来源参考：`/Users/hubertshelley/Documents/silent/tiangong-tabs-session/docs/tabs-session-binding.md`
>
> 当前分支：`feature/ephemeral-extension-tabs`

> 2026-09-03 最新决策：拓展区 Tab 不再写入会话记录，应用重启后不恢复。切换会话仍沿用隐藏语义，浏览器页面可在当前进程内按会话继续存活。本文后续旧持久化与跨重启恢复描述仅保留为历史设计记录。

## 完成标准

本功能完成时必须同时满足：

- 浏览器和终端共用一个工作区面板，同一条 Tab 栏内可以混排浏览器 Tab 与终端 Tab。
- 应用重启后打开任意会话时拓展区从空状态开始，浏览器也不得从磁盘恢复旧页面。
- 切换会话时收起原会话浏览器；切回后允许继续使用当前进程内仍存活的页面。
- 同一会话内可以打开多个终端 Tab，每个终端 Tab 拥有独立 PTY、cwd、输出历史和日志文件。
- 浏览器 Tab 只在需要真实页面时创建 WebView，`about:blank` 空白 Tab 不提前创建 WebView。
- `run_command` / `run_shell` 保持通过终端执行；执行前先检测当前会话终端状态，优先选择已打开且空闲的终端。
- 当前会话没有可用终端，或所有已打开终端都处于繁忙状态时，系统自动创建新终端执行本次命令。
- 如果本次命令是在自动新建的终端中执行，工具结果必须明确告知 Agent 没有复用旧终端。
- 用户关闭终端标签时前端必须立即完成关闭，不等待后台 PTY 退出。
- Terminal GC 必须回收前端已明确关闭、后台仍存活的终端。
- 本地验证至少通过 `cargo fmt -- --check`、`cargo check --workspace`、`yarn build`。

## 设计原则

- **拓展区状态仅驻留内存**：收起再展开及进程内会话切换可以继续使用内存状态；应用退出后不保存、不恢复。
- **插件运行实例独立管理**：宿主不再根据会话记录重建顶部 Tab；终端插件管理 PTY，浏览器插件管理 WebView。
- **前端只协调**：前端负责当前进程内的展示、切换和关闭，不承担跨会话持久化。
- **Agent 命令落在可见终端**：Desktop 模式下普通命令执行优先落在当前会话的存活终端里，用户可以看到命令过程。
- **关闭与回收解耦**：前端标签关闭是用户操作，不能被后台异常阻断；Terminal sidecar 负责最终回收对应 PTY。

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

## 当前状态边界

- Browser 标签和 WebView 只在当前进程内按会话保存，不再读取或写入 `browser-sessions`。
- 切换会话只隐藏原会话浏览器，不销毁当前进程中的页面。
- Terminal 标签不进入会话记录，后台残留由 Terminal 自身 GC 处理。
- Desktop 不再读取或写入 `workspace-tab-layouts`，也不提供会话标签读写接口。
- 浏览器全局历史和缩放设置属于独立功能，继续按原规则保存。
- 物理删除会话时仍清理旧版 `browser-sessions` 文件，避免遗留数据长期占用空间。

## 原始数据模型（已废弃，仅保留设计沿革）

以下 Core 字段和“不做一次性迁移”的兼容策略不再适用于当前实现。

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

### Core / App 命令（原始设计，已废弃）

位置：`src-tauri/src/commands.rs`

新增命令：

| 命令 | 作用 |
|------|------|
| `get_session_tabs(session_id)` | 读取会话持久化的统一 Tab 元数据 |
| `set_session_tabs(session_id, tabs, active_tab_id)` | 写入会话 Tab 元数据并持久化 |

要求：

- 当前实现不再读写 `Session.tabs` 和 `Session.active_tab_id`；该条仅记录旧设计。
- 反序列化失败时不得破坏原会话数据。
- 不在这些命令中创建 PTY 或 WebView。

### 终端插件

位置：`crates/plugins/tiangong-plugin-terminal`

运行时结构：

```text
SessionPtyRegistry
  sessions: HashMap<session_id, Arc<SessionTabs>>

SessionTabs
  tabs: HashMap<tab_id, SessionPty>
  active_tab_id: Option<String>
  activity: TerminalActivityTracker

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

位置：`crates/plugins/tiangong-plugin-browser`

新增能力：

| 命令 | 作用 |
|------|------|
| `browser_snapshot_tabs()` | 返回当前浏览器 Tab 快照 |
| `browser_switch_session(session_id, tabs_to_restore, active_tab_id)` | 切换浏览器运行时所属会话 |

关键规则：

- `BrowserManager` 记录当前绑定的 `active_session_id`。
- 切换会话时关闭旧会话 WebView，清理页面快照和待消费事件。
- 浏览器页面仅可从当前进程内的对应会话状态继续使用，不从磁盘恢复。
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
- 会话切换时从 Browser/Terminal 插件存储加载元数据，并按 Desktop 薄布局引用合并。
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

- 不读取会话标签记录，也不执行标签 hydrate。
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

### Terminal GC

- GC 请求固定为 `{ session_id: string, live_terminal_ids: string[] }`：`session_id` 是天工会话编号，`live_terminal_ids` 是该会话前端仍存活的终端编号全集。
- Terminal 页面按会话维护仍存在的终端标签编号集合；新建或关闭标签时向 sidecar 提交 `{ session_id, live_terminal_ids }` 完整集合。
- 标签切换、会话切换、拓展区隐藏和普通容器卸载不得从存活集合移除终端。
- sidecar 收到集合后立即结束同一会话中不在集合内的 PTY，并从运行表移除；不增加周期扫描任务。
- 存活集合只能影响所属会话，不得跨会话关闭终端。
- GC 完成后若该会话已无其他终端，清理对应恢复日志。
- 显式 `terminalClose` 仅用于工具主动关闭；前端强制关闭不依赖 GC 请求成功，失败由下一次新建或关闭触发重新对账。

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

## 任务拆分

任务 spec 已按可独立开发、独立验证的粒度拆分到 `docs/tabs-session-binding/` 目录：

1. `01-session-tab-model.md`：会话 Tab 数据模型
2. `02-session-tab-commands.md`：会话 Tab 读写命令
3. `03-terminal-registry-multitab.md`：终端多 Tab 注册表
4. `04-terminal-selection.md`：终端空闲选择与繁忙新建
5. `05-terminal-result-feedback.md`：命令结果反馈终端选择信息
6. `06-browser-session-switch.md`：浏览器会话切换
7. `07-browser-blank-lazy-webview.md`：浏览器空白页懒创建
8. `08-frontend-workspace-shell.md`：前端单一工作区面板
9. `09-frontend-tabs-container.md`：统一 Tabs 容器
10. `10-terminal-tab-content.md`：终端 Tab 内容组件
11. `11-browser-tab-content.md`：浏览器 Tab 内容组件
12. `12-session-restore-persistence.md`：会话切换恢复与防抖持久化
13. `13-permissions-and-api.md`：Tauri API 与权限声明
14. `14-end-to-end-verification.md`：端到端验收

## 验收清单

- [ ] 新会话打开终端按钮后出现一个终端 Tab。
- [ ] 同一会话可以新增多个终端 Tab，切换后 cwd 和输出互不覆盖。
- [ ] 新会话打开浏览器按钮后出现一个浏览器 Tab。
- [ ] 浏览器 `about:blank` 不提前创建 WebView，输入 URL 后正常打开页面。
- [ ] 同一个 Tab 栏内可以混排浏览器和终端。
- [x] 切换对话时收起原会话浏览器，当前进程内页面仍可继续使用。
- [ ] 重启应用后打开同一会话的浏览器，不出现旧页面。
- [ ] `run_command` / `run_shell` 在已有空闲终端时复用该终端执行。
- [ ] 当前会话没有可用终端时，`run_command` / `run_shell` 会先创建终端再执行。
- [ ] 当前会话所有终端繁忙时，`run_command` / `run_shell` 会新建终端执行。
- [ ] 新建终端执行时，工具结果会告知 Agent 本次使用了新终端。
- [ ] `terminal_send` 可以写入当前选中的可用终端。
- [x] 用户可强制关闭后台异常的终端标签，前端关闭不被 sidecar 错误阻断。
- [x] Terminal GC 在新建或关闭标签的对账中回收前端已关闭但后台仍存活的 PTY。
- [x] 切换会话或暂时隐藏标签不会触发 Terminal GC。
