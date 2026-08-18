# 插件 Harness 开发进度记录

> 关联：需求 `./requirements.md`、设计 `../plugin-harness-design.md`、任务总览 `./tasks/README.md`
> 开发分支：`feature/plugin-harness`

## 当前状态

- **阶段**：Harness 主体（T001-T016）完成；交互模型按新方案重做（T017-T019 已交付，
  T018 采用阻塞模型——按用户纠正，request_user 与 LLM 调用同为 turn task 外部 IO，
  不做挂起退出/续跑机制）。
- **当前建议任务**：真实使用冒烟（request_user 全链路 + 示例处理器插件导入）。
- **当前阻塞**：无。
- **当前阻塞**：无。
- **当前阻塞**：无。
- **下一步**：T007 → T008 → T009/T010 串行推进。

## 任务总览表

| 编号 | 任务 | 里程碑 | 状态 | 分支 | 提交 | 验证 | 遗留 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| T001 | Slot/Seam/Contribution 核心类型与注册表 | M0 | 已完成 | feature/plugin-harness | `585d1d74` | 见下方验证记录 | — |
| T002 | Manifest schema v2 解析与校验 | M0 | 已完成 | feature/plugin-harness | `585d1d74` | 见下方验证记录 | — |
| T003 | Host Bridge 后端命令层 | M0 | 已完成 | feature/plugin-harness | `585d1d74` | 见下方验证记录 | 事件源接入在 T007 后按需补 |
| T004 | settings.plugin-page Slot 前端容器 | M0 | 已完成 | feature/plugin-harness | `585d1d74` | 见下方验证记录 | GUI 手动冒烟待用户确认 |
| T005 | 端到端验证：旧插件经新桥接渲染设置页 | M0 | 已完成 | feature/plugin-harness | `585d1d74` | 见下方验证记录 | Memory 双向通信需 sidecar，GUI 冒烟待用户确认 |
| T006 | Shadow/iframe 沙箱容器组件 | M1 | 已完成 | feature/plugin-harness | `414f9c05` | 见下方验证记录 | 媒体资源代理、深度 JS 沙箱后续迭代 |
| T007 | extension.tab Slot 注册 + App 元数据 | M1 | 已完成 | feature/plugin-harness | `3cb5dbd9` | 见下方验证记录 | — |
| T008 | 顶部入口收敛 + 拓展区三态状态机 | M1 | 已完成 | feature/plugin-harness | `41a3a2d2` 等 | 见下方验证记录 | 矩阵图标映射留待 T016 | — | — | — | — |
| T009 | App 矩阵视图 + 启动台按钮 | M1 | 已完成 | feature/plugin-harness | `97eb63ca` | 见下方验证记录 | 三方绿点/数量徽标待实例感知 |
| T010 | singleton/multi 打开与实例管理 | M1 | 已完成 | feature/plugin-harness | `97eb63ca` | 见下方验证记录 | 真实三方插件 GUI 冒烟待 T016 |
| T011 | 浏览器插件化迁移 | M2 | 已完成（形态统一） | feature/plugin-harness | `f3147276` | 见下方验证记录 | 命令通道深度收敛渐进 |
| T012 | 终端插件化迁移 | M2 | 已完成（形态统一） | feature/plugin-harness | `f3147276` | 见下方验证记录 | 命令通道深度收敛渐进 |
| T013 | Agent Team 插件化迁移 | M2 | 已完成 | feature/plugin-harness | `f3147276` | 见下方验证记录 | 编排策略仍在 Core |
| T014 | 审批接缝 | M3 | 已完成 | feature/plugin-harness | `8e92b68d` | 见下方验证记录 | 风险分级策略待工具元数据扩展 |
| T015 | 交互接缝（选择/填写） | M3 | 已完成 | feature/plugin-harness | `07cddec7` | 见下方验证记录 | 三方交互处理器待插件生态 |
| T016 | SDK/脚手架/UI Kit/示例 | M4 | 已完成 | feature/plugin-harness | `d39d3be5` | 见下方验证记录 | UI Kit 组件库按需迭代 |

## 任务依赖表

| 编号 | 前置 | 后续 | 可并行 |
| --- | --- | --- | --- |
| T001 | 无 | T002, T003, T006, T014 | — |
| T002 | T001 | T004, T007 | T003 |
| T003 | T001 | T004, T006, T014, T015 | T002 |
| T004 | T002, T003 | T005 | — |
| T005 | T004 | T006 | — |
| T006 | T003 | T007 | — |
| T007 | T002, T006 | T008, T011-T013 | — |
| T008 | T007 | T009, T010 | — |
| T009 | T007, T008 | — | T010 |
| T010 | T007, T008 | — | T009 |
| T011-T013 | T007-T010 | — | 三者互可并行 |
| T014 | T003 | T015 | T006-T010 |
| T015 | T003, T014 | — | — |
| T016 | T001-T015 | — | — |

## 里程碑记录

| 里程碑 | 内容 | 状态 |
| --- | --- | --- |
| M0 | 接缝地基（T001-T005） | 已完成（2026-08-17，提交 `585d1d74`） |
| M1 | UI 接缝与能力矩阵（T006-T010） | 基本完成（T008 `41a3a2d2` 系列、T009/T010 `97eb63ca`；GUI 冒烟待三方制品） |
| M2 | 内置插件化（T011-T013） | 已完成（2026-08-18，`f3147276`） |
| M3 | 交互接缝（T014-T015） | 已完成（2026-08-17，`8e92b68d`/`07cddec7`） |
| M4 | 三方体验（T016） | 已完成（2026-08-18，`d39d3be5`） |

## 提交记录

| 提交 | 说明 |
| --- | --- |
| `a5cbfc41` | docs(plugin): 新增统一插件形态（Plugin Harness）设计方案 |
| `cbe28c7b` | docs(plugin): 新增插件 Harness 需求、任务 spec 与进度记录 |
| `585d1d74` | feat(plugin): 落地插件 Harness M0 接缝地基（T001-T005） |
| `ea18699f` | fix(plugin): bridge 权限校验对 v1 插件一律放行 plugin.*（M0 回归修复） |
| `414f9c05` | feat(plugin): T006 Shadow/iframe 沙箱容器组件 |
| `3cb5dbd9` | feat(plugin): T007 extension.tab Slot 注册与 App 元数据 |
| `41a3a2d2` 等 | feat(plugin): T008 顶部入口收敛 + 拓展区三态状态机（含矩阵内嵌、启动台图标、在用绿点、tab/矩阵右键菜单、绿点数据源修复等多轮迭代：`0c1ac6a3`、`a68cbc5f`、`1a367486`、`f22c8046`、`e74bec2b`） |
| `97eb63ca` | feat(plugin): T009/T010 三方 App 进矩阵与 plugin tab 实例管理 |
| `8e92b68d` | feat(plugin): T014 审批接缝——契约/路由骨架 + 超时 fail-closed + 始终允许 |
| `07cddec7` | feat(plugin): T015 交互接缝——ask_user 工具与 choice/form/confirm 交互 |
| `f3147276` | feat(plugin): M2 内置插件化——官方 App 进统一目录与 Agent Team 面板 |
| `d39d3be5` | feat(plugin): T016 三方体验——纯 UI 插件/SDK/脚手架/文档 |
| `b5ed0357` | fix(plugin): 浏览器改为多实例打开模式（审查前反馈） |
| `5af7faa6` | fix(plugin): 审查问题修复——iframe 桥接透传/事件订阅与始终允许运行时化 |
| `2b8801f0` | feat(core): T017 交互请求管理器与审批授权/挑战表 |
| `a1b9e9a1` 等 | feat(core): T018 交互模型改造——request_user 阻塞等待与挑战驱动审批（含非阻塞方案回退） |
| `eae12223` | feat(plugin): T019 交互模型入口与 UI——resolve_interaction 统一响应链路 |

（后续文档提交与代码提交分开记录）

## 验证记录

### T001-T004（2026-08-17）

- `cargo check -p tiangong-plugin-runtime`、`cargo check -p tiangong-app`、`cargo check --workspace` 通过。
- `cargo clippy -p tiangong-plugin-runtime --all-targets --tests` 零警告；`cargo fmt --all` 通过 pre-commit。
- 单元测试 33 项全绿（Slot Registry 合法/非法/前缀查询、Seam Hub 往返、manifest v2 正常/非法/v1 兼容/缺省值/native 签名、bridge 命名空间/权限/事件声明匹配）。
- 前端 `tsc --noEmit`、`yarn build`、`yarn test`（vitest 192 项）全部通过。

### T005 端到端（2026-08-17，`tests/m0_slot_bridge.rs`）

用真实 v1 WASM 制品完成闭环验证：

- **v1 清单按旧规则解析**：prompt（纯 WASM 无 sidecar）与 memory（声明 sidecar、制品缺失）两个 v1 插件均正常预加载。
- **settings.plugin-page Slot 贡献**：`list_slot_contributions` 正确合并两个 v1 插件的 WASM 贡献（source=wasm），memory 在 sidecar 不可用时仍保持加载、贡献可见。
- **设置页渲染**：`open_view` 对两个插件均返回非空 HTML。
- **双向通信闭环**：`bridge_call("prompt", "plugin.get_prompt/set_prompt", ...)` 完成读 → 写 → 读回，结果一致（真实写 `~/.tiangong/custom-prompt.md`，测试后恢复原值，无污染）。
- **拒绝行为**：未知 method（`rag.query`）被拒绝并给出可读错误；白名单内未接入命名空间（`session.*`）返回「尚未接入」。
- **回归**：`cargo test -p tiangong-plugin-runtime` 56 项全绿（含既有 load_and_call 15 项、signature 7 项）；前端 192 项测试全绿。

**遗留说明**：

1. Memory 的 view message（bootstrap/save_config）依赖 sidecar 进程，测试环境无法拉起真实 sidecar；其双向通信经 prompt 插件（同一 v1 兼容路径 + 同一 `plugin.*` 桥接通道）验证等价，Memory 侧待 GUI 手动冒烟确认。
2. GUI 手动冒烟（设置 → 插件：Memory/prompt 设置页加载、交互、主题切换、无 console 报错）需在桌面 App 中人工确认，前端桥接代码路径已被集成测试覆盖。
3. 事件订阅（bridge.on）当前为登记骨架，事件源接入在 T007 之后按需补充。

### M0 回归修复：v1 非空权限插件设置页误拒（2026-08-17，`ea18699f`）

用户 GUI 冒烟发现 generate-image-openai 设置页报错「未声明权限 bridge.call」。
根因：bridge 权限校验原实现按「v1 + permissions 非空即按声明校验」处理，
而 v1 清单早于 bridge 权限体系，不可能声明 bridge.call。

修复：

- `plugin.*` 命名空间对 v1 一律放行（等价旧 plugin_call 透传通道，零改动兼容）；v2 仍按声明校验。
- 其余宿主能力命名空间仅 v2 可达，v1 调用时明确提示需升级清单。
- 单元测试与 m0 端到端测试补充该回归场景；用本机真实安装的 generate-image-openai 验证 `plugin.bootstrap` 经新桥接正常返回配置。
- 修复后 `cargo test -p tiangong-plugin-runtime` 56 项全绿，clippy 零警告。

### 遗留问题（待后续任务处理，不阻塞 M1）

1. **mcp 插件设置页布局异常**（2026-08-17 GUI 冒烟发现，与 Harness 修改无关）：MCP 服务器连接失败时（如 dbx、brave-search），页面内错误文本过长无折行/截断，溢出服务器条目与右侧操作按钮重叠。属 mcp 插件页面自身样式缺陷（iframe 容器与桥接通道行为正常，数据可正常读写），后续在插件侧修复。
2. 事件订阅（bridge.on）为登记骨架，事件源接入在 T007 之后按需补充。

### T006（2026-08-17，`414f9c05`）

- `cargo check -p tiangong-app`、`cargo clippy -p tiangong-plugin-runtime --all-targets --tests` 零警告；`cargo test -p tiangong-plugin-runtime` 57 项全绿（新增 v2 manifest 贡献链路集成测试：Slot 列出/entry 读取/资源读取/`../` 逃逸拒绝）。
- 前端 `tsc --noEmit`、`yarn build`、`yarn test` 200 项全绿（新增沙箱容器组件测试 8 项：shadow 挂载、内联/外链脚本受控执行、外链样式注入、桥接 call 转发、bridge.on 按 plugin_id 分发、卸载退订清理、:host token 注入、iframe/native 分发）。
- 任务 spec：`./tasks/006-沙箱容器组件.md`、`./tasks/007-extension-tab注册与App元数据.md`、`./tasks/008-顶部入口收敛与三态状态机.md`、`./tasks/009-010-三方App矩阵与实例管理.md`。

### T008-T010（2026-08-17，`41a3a2d2`…`97eb63ca`）

- T008：顶部「终端/浏览器」收敛为单个「拓展区」按钮；三态状态机（关闭/矩阵/App）；
  矩阵态 tab 栏保留（启动台按钮高亮）、App 实例隐藏保活；启动台图标网格与「在用」
  绿点（会话存在实例即亮，与按钮绿点同源）；tab 右键菜单（多实例新建/关闭其他/关闭，
  单实例仅关闭，移除「新建浏览器」按钮）；矩阵 App 右键菜单（打开/聚焦、新建实例、
  关闭全部实例）。状态机组件测试 2 项；绿点数据源经多轮迭代收敛为
  onTabKindsChanged 即时通知（修复新对话不亮、关闭残留、落盘竞态误灭三个缺陷）。
- T009/T010：后端布局层支持 plugin tab（kind/元数据/持久化往返测试）；前端 plugin
  tab 打开按 open_mode 分派、内容经 PluginAppTabContent + PluginSandbox 渲染；
  矩阵接入 listExtensionApps 三方卡片。
- 验证：后端布局层测试 4 项、前端 202 项测试全绿；`cargo check -p tiangong-app`、
  `yarn build` 通过。
- 遗留：三方 App 绿点/数量徽标与右键关闭（需 plugin 实例集合感知）、三方图标映射、
  真实三方插件的 GUI 冒烟（待 T016 示例制品）。

### T014/T015（2026-08-17，`8e92b68d`/`07cddec7`）

- T014 审批接缝：approval.rs 契约与 ApprovalRouter 骨架（scope 前缀匹配/卸载回滚，
  4 项测试）；Core 审批等待超时 fail-closed（默认 300s，TurnContext 可配置）；
  会话级「始终允许」（Session.approved_tools 持久化，审计标签区分）；桌面卡片与
  CLI 增加「始终允许」。集成测试：超时拒绝闭合、始终允许后二次调用免审。
- T015 交互接缝：interaction.rs 契约；InteractionNeeded 事件与 Command::Interaction；
  Core 内置 ask_user 工具（choice/form/confirm，挂起-恢复与审批同款机制，超时/取消
  fail-closed）；桌面 InteractionCard 三种交互渲染；CLI 文本交互。集成测试：
  choice 响应恢复、超时闭合。
- 任务 spec：`./tasks/014-审批接缝.md`、`./tasks/015-交互接缝.md`。
- 验证：Core react 46 项、plugin-runtime 38 项、前端 202 项全绿；clippy 零警告；
  `cargo check -p tiangong-app` 通过。既有 flaky（steering_message，改动前即失败）记录在案。
- 遗留：审批风险分级策略（tool-spec dangerous 元数据）待 WIT 元数据扩展；三方审批/交互
  处理器的桥接路由待插件生态（契约与骨架已就位）；交互 UI 超时倒计时展示待真实使用反馈。

### T011-T013（2026-08-18，`f3147276`）

- 官方 App 统一注册：浏览器/终端/Agent Team 以 `__builtin__` 官方身份进入
  list_extension_apps（official 标记、置顶、native 容器、声明化 open_mode），
  矩阵统一渲染官方 + 三方（移除前端硬编码卡片）。
- Agent Team 官方 App：AgentTeamPanel（子 Agent 实时状态：活跃标记、上下文/累计
  token），plugin tab 的 native 容器按贡献分派；编排调度保留在 Core（设计 8.3）。
- 浏览器/终端以「形态统一」交付：App 目录声明化 + 既有 native 容器与打开路径；
  plugin:browser|* 等内部命令通道的桥接化收敛作为渐进项（native 容器本就是
  官方专属通道，无特权外泄）。
- onTabKindsChanged 扩展上报 plugin App 键集合：三方/官方 plugin App 矩阵绿点
  （补 T009 遗留项）。
- 验证：官方 App 目录测试（置顶/模式/native）、状态机测试适配、前端 202 项全绿、
  clippy 零警告。

### T016（2026-08-18，`d39d3be5`）

- 纯 UI 插件（设计 9.1）：manifest v2 wasm 可省略，registry 全链路支持；「UI 优先」
  开发模型落地——只会 JS/TS 也能做拓展区 App。
- storage.* 宿主路由（设计 7.8）：get/set/delete/list 落盘插件私有 data 目录，纯 UI
  插件即有持久化。
- @tiangong/plugin-sdk：类型 + createTiangongBridge（Shadow/iframe 双容器适配）+
  pluginStorage 封装。
- 脚手架：xtask new-plugin 生成看板示例骨架（无构建原生 JS），导入即用。
- 文档：plugin-development.md v2 章节。
- 验证：纯 UI 插件端到端测试（安装/目录/贡献/storage 往返/落盘/plugin.* 拒绝），
  后端 68 项全绿，clippy 零警告。
- 遗留：@tiangong/plugin-ui-kit 组件库按需迭代；npm 发布渠道待定；示例插件的
  GUI 手动冒烟待用户执行。

### 交互模型重做（2026-08-18，T017-T019，方案 `./interaction-model-redesign.md`）

按新方案重做审批与交互（替代 T014/T015 实现）：
- T017 `2b8801f0`：InteractionRegistry（原子闭合/绝对 deadline/竞态唯一赢家）、
  ApprovalGrants（Once 参数绑定/Runtime 跨 turn）、ApprovalChallenges（一次性消费）。
- T018：request_user 统一工具（六 kind、独占批次、15s fail-closed 超时）、
  挑战驱动审批（Supervised 无授权返回 approval_required；approve_once/Runtime 授权）、
  删除 wait_approval/wait_interaction/旧命令与 Session.approved_tools。
  **关键决策（用户纠正）**：采用阻塞等待模型（工具等待用户 = turn task 等待外部 IO，
  与等待 LLM 同构）；方案中的非阻塞（挂起退出+续跑）在天工常驻 turn task 架构下
  属对抗性设计，已回退。
- T019 `eae12223`：resolve_interaction 统一响应命令/前端六 kind 交互卡片/
  CLI 文本交互/agent-team 上抛/后台通知。
- 验证：Core 98 项（审批闭环/拒绝闭环新用例通过；steering 为既有并发 flaky）、
  interaction 10 项、前端 204 项全绿。
- T020（2026-08-18，`6e4ca6f9`）：交互处理器插件化完成（方案 v2）——session.interaction
  Slot、interaction.resolve 桥接（权限双校验+宿主注入）、interaction.requested/closed 事件、
  request_id 权威会话路由、deadline 锁内原子判定、approval 强制显式 challenge、
  InteractionPluginHost 替换内置卡片、SDK createInteractionHandler、CLI/Server 宿主接口、
  示例插件移至 plugins/interaction-handler-example（文档路径引导）。
  验收测试补 4 项；Core 102 / runtime 42 / 前端 204 全绿（steering 为 origin 既有 flaky）；
  另修 tiangong-llm 测试构造缺 Message 耗时字段的既有编译失败。

### 代码审查修复（2026-08-18，`5af7faa6`）

审查发现 3 项，全部修复：
1. iframe 桥接强制改写 `plugin.*`（高）：改为按命名空间透传，裸方法名才补前缀
   （v1 设置页兼容）；双容器桥接行为一致，回归测试 2 项。
2. iframe 事件订阅空操作（中）：打通 subscribe 消息对接与 bridge_event 回推，
   SDK/模板 on/off 完整实现。
3. 始终允许随会话文件永久保存（中）：approved_tools 改 serde(skip) 仅运行期有效，
   重启恢复审批确认；文案改「本次运行内」；序列化断言。
- 验证：前端 204 项（新增 2）、Core react 46 项、runtime 68 项全绿；clippy 零警告。
- 审查另确认：Core 网络集成测试并发串线为 origin/main 既有问题，非本分支引入。

### T007（2026-08-17，`3cb5dbd9`）

- `cargo test -p tiangong-plugin-runtime` 58 项全绿（新增 extension.tab 聚合用例：multi 显式/singleton 缺省、descriptor 名作为 App 名、settings 贡献不进入 App 列表）。
- clippy 零警告；前端 `tsc --noEmit`、`yarn build` 通过。

## 更新规则

1. 每完成一个任务：更新状态、分支、提交、验证结果、遗留问题。
2. 每解决一个阻塞：更新「当前阻塞」与「下一步」。
3. 发现设计不一致：先回改对应 spec，再更新本文件，不直接猜实现。
4. M2 及之后任务 spec 在 M1 完成后逐批细化，细化时同步更新任务总览。
