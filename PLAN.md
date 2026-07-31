# 天工项目计划

## 项目目标与愿景

天工定位为可在桌面、命令行和 Server 模式下统一运行的个人智能终端。核心能力包括对话、任务执行、定时任务、插件扩展及通过外部通讯工具进行远程控制。

## 技术选型与架构

- 后端使用 Rust workspace，核心能力按 crate 拆分。
- 桌面端使用 Tauri，前端使用 React、TypeScript 与现有 shadcn/ui 风格组件。
- 插件平台逐步迁移到单文件 WASM Component；Core 保留 Agent Loop，具体能力通过受限的宿主接口接入。
- Memory 当前采用 WASM 插件桥接原生 MemoryHandle/sidecar 的过渡方案，完整逻辑下沉与 Core 解耦分阶段完成。
- 三方 IM 通过独立 bot 制品接入，由天工负责下载、配置、启动和监控；bot 通过 Server API 与天工通信。
- 配置 schema、平台专属授权流程及扫码所得凭证均由 bot 制品负责，天工只负责通用命令调用和状态展示。

## WASM 插件 / Sidecar 架构

插件由 WASM 组件与配套 sidecar 共同组成完整能力：

- **WASM 与 sidecar 共同组成完整插件**，插件包支持携带多平台 sidecar 二进制。
- **App 统一管理 sidecar 生命周期**（安装、启动、健康检查、崩溃恢复、关闭），不再由插件自行选举或启动。
- **WASM 通过通用 Host 接口调用自己的 sidecar**，Host 根据当前 WASM 实例自动路由，WASM 不传插件 ID 或地址。
- **Sidecar 通过 Host 模型代理调用外部模型**，不直接持有模型密钥或 URL。
- **Host 不理解插件业务协议**，只转发原始字节负载。
- **通用插件协议不得依赖 Memory 专用结构**，Memory 私有协议由插件自己维护（共享 crate）。
- 模型调用链：Memory WASM → Host sidecar transport → Memory Sidecar → Host LLM Proxy → LLM / Embedding / Rerank 服务。
- 配置 schema、平台专属授权流程及扫码所得凭证均由 bot 制品负责，天工只负责通用命令调用和状态展示。

## 功能优先级

- P0：保证桌面端、CLI、Server、会话与配置能力稳定可用。
- P1：推进 Issue #301 插件平台 PoC 与 Issue #321 Memory WASM 试迁移，先保证三入口和真实记忆链路稳定。
- P1：完成 Issue #250 通讯网关闭环，并完成 Issue #270 QQ Bot 官方接入与直接扫码配置。
- P2：将定时任务结果推送到指定通道，并继续扩展其他 IM 平台。

## 版本里程碑

| 里程碑 | 目标 | 状态 | 时间窗口 |
| --- | --- | --- | --- |
| v0.12 | 完成全栈基础能力与 Server API 基线 | 已完成 | 2026 年 7 月前 |
| v0.13 | 完成 Issue #250 的飞书、微信通讯网关，并接入支持直接扫码配置的 QQ Bot | 进行中 | 2026 年 7 月 |
| 插件化 PoC | 跑通单文件 WASM、Memory 真实链路和动态设置页，明确后续热加载与解耦边界 | 进行中 | 2026 年 7 月起 |
| 后续版本 | 增加更多 IM 适配器，并接入定时任务结果推送 | 待规划 | v0.13 之后 |

## 当前阶段

当前聚焦 Issue #250 与 Issue #270。飞书、微信和 QQ bot 制品，以及运行管理、展示名称、配置删除、日志查看、升级、天工服务配置自动注入、扫码授权、Bot ID 路径安全、入站图片转发、本地文件回传、运行状态一致性及开发模式退出链路均已完成；三端 Bot 已具备 MCP 文本、图片和文件推送能力，由 Bot 自动维护并授权主动发过消息的多目标清单，MCP 注册和注销绑定到 Bot 的启动与停止流程，三个独立 Bot 工程也已纳入 CI 检查。

Issue #301 与 #321 已进入试开发：WASM 组件加载、Memory host request、sidecar 启动管理、三入口注册、生命周期桥接、内嵌设置页和运行稳定性修复已经落地。现有 Memory 专用模型配置与数据管理页面已完整迁入 WASM 插件 UI，宿主只保留通用页面容器和消息桥接；热加载、权限探测、右侧通用 Tab、安装升级和完整 Core 解耦仍按后续独立任务推进。


## 跨平台嵌入浏览器凭据能力（验证阶段）

- 在不读取系统浏览器密码明文的前提下，验证 Windows WebView2 与 macOS WKWebView 的用户名密码保存、建议、填充和通行密钥能力。
- Windows 优先实现 WebView2 密码自动保存，并保持已验证可用的 WebAuthn 与 Windows Hello 能力。
- macOS 优先验证系统 Password AutoFill；完整 WebAuthn 依赖 Apple 浏览器公共密钥凭据受限能力与正式签名。
- 系统能力不足时提供默认浏览器登录兜底，不使用脚本截获或注入密码。
- 详细范围、风险和完成标准见 `docs/browser/06-credential-capability-validation.md`。

## macOS 嵌入式浏览器稳定性（当前热修）

- 修复 WKWebView 导航期间 URL 暂时不可用导致桌面进程退出的问题，保持 Windows、Linux 和移动端依赖行为不变。
- 在 wry 上游正式发布修复前，仅通过精确提交保留空 URL 防崩溃补丁，不继续扩展 wry 的失败处理。
- 天工对每次导航使用固定截止时间；截止前没有完成时统一显示页面加载异常，避免浏览器持续白屏。
- 错误页、标签地址、浏览历史和 Agent 结果必须使用同一导航状态，避免把失败内容误判为成功页面。
- 上游发布空 URL 修复后先回归无效 URL、页面加载和浏览器工具流程，再移除临时补丁。
- 详细方案见 `docs/browser/07-navigation-failure-recovery.md`。
