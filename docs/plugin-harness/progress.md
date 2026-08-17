# 插件 Harness 开发进度记录

> 关联：需求 `./requirements.md`、设计 `../plugin-harness-design.md`、任务总览 `./tasks/README.md`
> 开发分支：`feature/plugin-harness`

## 当前状态

- **阶段**：M0 已完成；M1 进行中（T006 已完成，T007 进行中）。
- **当前建议任务**：T007（extension.tab Slot 注册 + App 元数据）。
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
| T007 | extension.tab Slot 注册 + App 元数据 | M1 | 未开始 | — | — | — | — |
| T008 | 顶部入口收敛 + 拓展区三态状态机 | M1 | 未开始 | — | — | — | — |
| T009 | App 矩阵视图 + 启动台按钮 | M1 | 未开始 | — | — | — | — |
| T010 | singleton/multi 打开与实例管理 | M1 | 未开始 | — | — | — | — |
| T011 | 浏览器插件化迁移 | M2 | 未开始 | — | — | — | — |
| T012 | 终端插件化迁移 | M2 | 未开始 | — | — | — | — |
| T013 | Agent Team 插件化迁移 | M2 | 未开始 | — | — | — | — |
| T014 | 审批接缝 | M3 | 未开始 | — | — | — | — |
| T015 | 交互接缝（选择/填写） | M3 | 未开始 | — | — | — | — |
| T016 | SDK/脚手架/UI Kit/示例 | M4 | 未开始 | — | — | — | — |

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
| M1 | UI 接缝与能力矩阵（T006-T010） | 进行中（T006 完成，`414f9c05`） |
| M2 | 内置插件化（T011-T013） | 未开始 |
| M3 | 交互接缝（T014-T015） | 未开始 |
| M4 | 三方体验（T016） | 未开始 |

## 提交记录

| 提交 | 说明 |
| --- | --- |
| `a5cbfc41` | docs(plugin): 新增统一插件形态（Plugin Harness）设计方案 |
| `cbe28c7b` | docs(plugin): 新增插件 Harness 需求、任务 spec 与进度记录 |
| `585d1d74` | feat(plugin): 落地插件 Harness M0 接缝地基（T001-T005） |
| `ea18699f` | fix(plugin): bridge 权限校验对 v1 插件一律放行 plugin.*（M0 回归修复） |
| `414f9c05` | feat(plugin): T006 Shadow/iframe 沙箱容器组件 |

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
- 任务 spec：`./tasks/006-沙箱容器组件.md`。

## 更新规则

1. 每完成一个任务：更新状态、分支、提交、验证结果、遗留问题。
2. 每解决一个阻塞：更新「当前阻塞」与「下一步」。
3. 发现设计不一致：先回改对应 spec，再更新本文件，不直接猜实现。
4. M2 及之后任务 spec 在 M1 完成后逐批细化，细化时同步更新任务总览。
