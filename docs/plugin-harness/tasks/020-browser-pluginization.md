# 020 浏览器插件化

> 状态：进行中
> 分支：`feature/browser-plugin`
> 关联：`docs/plugin-harness-design.md`（Everything is a Plugin）、`plugins/browser-handler`

## 1. 目标

嵌入式浏览器从内置实现（`crates/plugins/tiangong-plugin-browser` + 前端 `BrowserPanel/BrowserTabContent`）迁移为 `browser-handler` 插件，对标终端插件化终态：**宿主零浏览器界面与工具代码**，浏览器能力以插件形态交付。

- 页面引擎保留宿主原生 webview（容器原语 `webview.*`，按插件隔离实例）；
- 管理界面（地址栏/导航）在插件 UI（shadow 容器），页面标签统一由 App 拓展区顶部标签承载；
- Agent 浏览器工具由插件 TS 壳提供（策略在插件，引擎经协作原语）。

## 2. 架构接缝

| 层 | 职责 | 现状 |
|----|------|------|
| `webview.*` 容器原语（宿主注入） | 实例创建/导航/eval/位置对齐/标签管理/页面协作 | create/navigate/eval/hide/close + setPosition/show/tabs/tabNew/tabSwitch/tabClose/back/forward/reload + fetch/queryDom/click/formFill/formExtract/locate |
| `browser-handler` 插件 UI | 单个页面的地址栏、导航和位置同步（内容区矩形 → 对应 webview 实例） | 本任务实现 |
| `browser-handler` 工具壳 | browser_open/navigate/eval + web_* 协作工具（映射到原语） | 雏形已有，随迁移完善 |
| 宿主 `sandbox: webview` 分发 | 管理界面渲染进 shadow 容器（主文档坐标可同步原生 webview 位置；iframe 拿不到主窗口坐标） | 本任务实现 |

## 3. 任务分解

- [x] 引擎：webview 原语补管理界面所需方法（setPosition/show/tabs/tabNew/tabSwitch/tabClose/back/forward/reload）。
- [x] 宿主：`sandbox: webview` 贡献分发到 shadow 容器渲染。
- [x] 插件 UI 最小闭环：SDK 桥接入、地址栏导航、后退/前进/刷新、webview 位置跟随内容区（ResizeObserver + 窗口事件）、面板卸载隐藏（会话保留、重开恢复）。
- [x] 会话隔离（对齐终端插件）：原语 scope 升级为插件×会话双维度（`webview:<插件>:<会话>`），面板跟随对话切换标签集（旧会话隐藏保留、新会话恢复/空态）；工具调用绑定发起会话（Agent 页面与该对话面板同实例）；协作原语统一注入调用方 scope（修复此前缺省落到 `webview:default`、工具与面板各看各页的缺陷）。
- [x] 标签归属纠正：删除插件内部标签栏，每个浏览器页面映射为 App 拓展区顶部标签，与终端标签共同排序、切换和关闭。
- [x] 显隐纠正：原生页面严格跟随顶部当前标签；切换到终端时隐藏浏览器页面，并让终端重新显示时刷新尺寸与画面。
- [x] 插件 UI 第二阶段：以迁移前浏览器界面为基准对齐工具栏、地址栏、明暗主题和窄屏布局。
- [x] 交互对齐：页面标题/地址/加载状态实时刷新，后退与前进可用状态、历史记录、缩放及快捷键、批注与结果弹层全部恢复。
- [x] 加载完成修正：页面已经正常显示时可靠结束加载状态，不再被 30 秒截止时间改成失败页面。
- [ ] 会话删除时的 webview 实例回收（避免长会话场景下隐藏实例累积）。
- [ ] 工具壳完善：结果摘要格式化、错误语义对齐原版工具。
- [ ] 宿主下线内置浏览器界面：拓展区「浏览器」入口切到插件贡献，`BrowserPanel/BrowserTabContent` 退役。
- [ ] Agent 会话联动：浏览器事件（网络/页面状态）注入对话链切换到插件工具路径。

## 4. 非目标（当前阶段）

- Chrome 内核/外部浏览器后端：经实测系统 webview 无密码自动填充与 Passkey，真浏览器能力（凭据/Passkey/扩展）需外联本机 Chrome（CDP）方案，另行立项。
- 密码凭据注入（钥匙串读取 + formFill）：依赖上述外部浏览器方向决策。

## 5. 验证

- `cargo check/clippy -p tiangong-plugin-browser`；
- 插件 `yarn build`（vue-tsc + vite）；
- `yarn package` 本地签名打包；功能验收由用户在 Desktop 完成。
- 与迁移前浏览器界面逐项对照，确认浏览器没有内部标签栏，页面直接出现在 App 拓展区顶部标签，并可与终端正常切换。

2026-08-20：完成标签归属、原生页面显隐和加载完成判定修正；浏览器插件、终端插件和宿主前端构建通过，前端 210 项、终端插件 3 项及浏览器引擎 34 项现有检查通过；两个插件本地包均已重新生成，终端包签名有效；Desktop 功能由用户验收。
