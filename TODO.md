# 手动上下文管理状态修复

- [x] 记录手动上下文操作，结果到达后恢复当前会话为空闲状态。
- [x] 保持自动上下文压缩期间当前任务继续执行。
- [x] 清除手动上下文操作开始前遗留的上一轮耗时。
- [x] 取消手动上下文操作后清除运行标记和提示，不留下取消痕迹。
- [x] 确保手动压缩结果是本次操作的最后一条状态消息。
- [x] 通过 Rust 检查和前端正式构建。

## 完成标准

- 手动压缩成功、无需压缩、失败或取消后均可继续输入，取消后不保留提示。
- 手动清理完成后可继续输入。
- 自动压缩完成或失败不会提前结束正在执行的任务。
- 手动操作期间不显示上一轮任务耗时。

# #301 / #321 WASM 插件平台与 Memory 试迁移

## 已完成基础链路

- [x] 定义并构建单文件 WASM Component，提供插件描述、工具、Prompt、生命周期和设置页接口。
- [x] 引入 Wasmtime Component Model，并设置 fuel、内存限制和 epoch deadline 配置骨架。
- [x] 通过通用 sidecar 接口把 Memory 调用转发到独立进程。
- [x] 增加 Memory sidecar 二进制和启动管理，移除进程内降级。
- [x] Desktop、CLI、Server 三入口加载同一 Memory WASM 制品。
- [x] 前端根据插件 contribution 动态显示 Memory 设置入口，并从 WASM 内存中加载页面。
- [x] 移除原有硬编码 Memory 设置入口，试迁移阶段由 WASM 插件接管 Memory 工具与提示注入。

## 当前任务：运行稳定性闭环

- [x] 修复未完成代码导致的插件运行时编译失败。
- [x] 统一插件加载、配置、生命周期、工具、Prompt 和页面调用的执行边界，避免异步环境内嵌套运行崩溃。
- [x] 多个 Memory WASM 实例复用同一 sidecar 连接和进程。
- [x] 插件调用异常由边界转换为普通错误，不直接导致宿主进程退出。
- [x] 重新构建 WASM 制品并通过现有插件运行时验证。
- [x] 通过 Desktop、CLI、Server Rust 检查与前端正式构建。
- [x] 启动 Desktop，确认插件预加载和 sidecar 自动启动；退出后 App 与 Vite 无残留，独立 sidecar 保持服务。
- [x] 使用真实浏览器验证插件设置一级入口、内嵌页面、配置读取与保存消息桥接；原生窗口自动点击仍受系统权限限制。

### 当前任务完成标准

- 工作区能够正常编译，WASM 制品能够重新生成并加载。
- 创建会话、构建系统提示、调用记忆、轮次结束和打开插件设置页均不再触发运行时崩溃。
- 多个插件实例共用同一 Memory sidecar，不随会话数重复启动进程。
- 现有插件运行时验证、三入口检查和前端正式构建通过。

## 当前任务：Memory 插件 UI 完整迁移

- [x] 将现有 Memory 文本、嵌入、重排模型选择和向量模式迁入 WASM 插件页面。
- [x] 将 Memory 统计、搜索、状态筛选、图谱与列表预览迁入 WASM 插件页面。
- [x] 将分页或增量加载、刷新和召回测试迁入 WASM 插件页面。
- [x] 将手工记忆新增编辑、单条与批量归档恢复迁入 WASM 插件页面。
- [x] 将记忆关系查看、新增和删除迁入 WASM 插件页面。
- [x] 页面数据操作统一经插件消息和通用 sidecar 接口完成，宿主不增加 Memory 专用页面逻辑。
- [x] 移除宿主中已被插件页面替代的 Memory 专用界面代码和直接调用入口。
- [x] 重新构建 Memory WASM，并只验证 Memory 相关检查与完整页面流程。

### Memory 插件 UI 迁移完成标准

- 插件页面能够读取候选模型和当前 Memory 配置，保存后真实 Memory 配置立即生效。
- 现有 Memory 数据查看、召回测试、编辑归档和关系管理能力均可在插件页面使用。
- 宿主设置页只负责动态入口、iframe 容器和通用消息转发，不解析 Memory 业务数据。
- Memory WASM 制品、插件运行时验证和 Memory 页面真实浏览器流程通过。

## 当前任务：Memory 插件 UI 视觉与交互对齐

- [x] 通用 iframe 容器下发宿主主题和受控样式变量，并通过独立通道校验插件消息来源与响应。
- [x] Memory 页面跟随宿主明暗主题，移除独立品牌色、装饰网格和彩色统计顶边。
- [x] 对齐按钮、输入框和下拉框尺寸，并为插件贡献图标增加受控 Lucide 映射。
- [x] 恢复配置自动保存、加载状态和文本模型 `chat` / `lite` 候选，避免并发保存覆盖新选择。
- [x] 恢复关系目标搜索、键盘选择、批量读取失败降级和关系去重。
- [x] 恢复图谱缩放、平移、选中节点操作、邻接高亮和拖动点击隔离。
- [x] 完善召回弹窗焦点管理、失败数据保留和筛选失败回退。
- [x] 通过前端正式构建、主程序检查、Memory WASM 检查和完整插件制品构建。

### 当前任务完成标准

- 插件页面主题、字体、表单控件和图标与宿主设置页一致，明暗主题切换后立即更新。
- 配置、关系、图谱和弹窗交互不低于迁移前页面，失败时不会覆盖新配置或误操作旧筛选数据。
- iframe 保持脚本沙箱隔离，只处理当前窗口和当前通道的合法消息。
- Memory 页面源码、内嵌 WASM 页面和 sidecar 制品均能够正常构建。

## 当前任务：Memory 插件归档与动态热加载

- [x] 将 Memory WASM、sidecar、私有协议和 `plugin.json` 统一整理到 `crates/plugins/tiangong-plugin-memory/`。
- [x] 删除未使用的进程内原生 Memory 插件和旧目录引用，三个子目录继续作为根 Workspace 的独立成员。
- [x] 建立只依赖序列化库的 Memory 私有协议，集中定义业务版本、操作名称、请求、响应和业务错误。
- [x] WASM 与 sidecar 共同依赖私有协议，调用负载不再重复携带操作名称，App 不依赖私有协议。
- [x] 清单运行文件路径由插件 ID 固定生成，不允许插件自定义 endpoint 和日志路径。
- [x] 提供 `cargo run -p xtask -- build-plugin memory`，一次构建并部署完整插件包。
- [x] 运行时提供插件状态查询与动态热加载；新版本完成实例化后再替换，失败时保留旧实例。
- [x] 设置中增加插件管理页面，显示插件版本、运行状态和 sidecar 信息，并支持刷新与热加载。
- [x] 热加载后刷新插件贡献页面，当前存活 Core 后续调用切换到新实例。
- [x] 只执行 Memory、插件运行时、桌面入口和设置页相关验证。

### 当前任务完成标准

- Memory 插件三部分和清单位于同一目录，旧目录及未使用原生插件已清理。
- 私有协议可同时通过本机与 `wasm32-wasip2` 编译，WASM 与 sidecar 请求类型一致。
- 一次构建命令生成并部署完整插件，运行目录不会越过自身插件目录。
- 设置页可查看 Memory 插件并完成真实热加载，加载失败不会中断当前可用版本。
- 现有会话和插件页面在热加载后使用新实例，Memory 配置与数据页面仍可正常打开。

## 当前任务：基于 OSS 的插件完整管理

- [x] 定义 OSS 静态插件目录，包含插件版本、说明、WASM 制品和当前平台 sidecar 的下载地址及 SHA-256。
- [x] 设置页同时显示已安装与 OSS 可安装插件，能够刷新远端目录和本地运行状态。
- [x] 支持从 OSS 下载并原子安装插件，校验清单 ID、版本、制品名称和 SHA-256。
- [x] 支持插件启用和停用，当前存活 Core 与后续新 Core 均立即使用新的启用状态。
- [x] 支持检查升级、下载新版本、失败恢复旧版本，并允许回滚到上一个本地版本。
- [x] 支持卸载插件，并由用户选择删除或保留插件数据。
- [x] Memory 统一构建命令生成可上传 OSS 的完整制品及静态目录片段。
- [x] 设置页操作已全部接通，并在隔离的 Memory 插件目录完成安装、启停、热加载、升级、回滚和卸载真实流程验证。
- [x] 只执行插件运行时、Memory、桌面入口和插件管理页面相关验证。

### 当前任务完成标准

- 不依赖插件平台即可通过 OSS 静态目录发现、安装和升级插件。
- 下载或加载失败不会破坏当前可用版本，升级后可回滚到上一个版本。
- 停用插件后不再提供页面、工具和提示能力，重新启用无需重启 App。
- 卸载操作不会误删其他插件目录，保留数据时后续重装可继续使用。

## 当前任务：本地插件导入与开发说明

- [x] 设置页能够选择包含 `plugin.json`、WASM 和当前平台 sidecar 的本地完整插件目录。
- [x] 本地制品先复制到受管临时目录，再执行清单、版本、WASM 和 sidecar 校验；失败时不改变当前插件。
- [x] 本地导入支持安装新插件和重新导入同版本插件，不允许用更低版本覆盖当前插件。
- [x] 本地重新导入复用现有数据保留、原子替换、失败恢复和回滚能力。
- [x] 在 `docs/` 编写插件开发说明，并从项目 README 和插件管理页连接到 GitHub 文档。
- [x] 只执行插件运行时、桌面入口、插件管理前端和本地导入流程相关验证。

### 当前任务完成标准

- 用户可以从设置页导入本地插件，无需手工复制到应用数据目录。
- 无效目录、缺失制品、版本不一致或加载失败均返回明确错误，现有插件继续可用。
- 开发者能够按文档完成目录组织、清单编写、WASM 构建、可选 sidecar 接入和本地调试。
- 设置页的开发文档入口能够打开 GitHub 中的权威文档。

## 后续阶段

- [ ] 实现零权限探测、授权实例化和权限扩大确认。
- [ ] 启动 epoch 心跳并验证墙钟超时，补齐 host request 超时与取消传播。
- [ ] 完成不可变版本快照、旧调用排空、页面版本绑定、关闭清理和重启恢复。
- [ ] 完成通用右侧 Plugin Tab。
- [ ] 补齐插件配置与 Secret 独立命名空间，以及页面资源和消息的完整限制。
- [ ] 将 Memory 的提取、整理和反刍编排逐步下沉到 WASM，使 sidecar 最终只保留原子存储能力。
- [x] 移除 Core 对原生 Memory 插件和具体 Memory crate 的静态业务依赖。
- [ ] 建设插件发布平台，并补齐制品签名、发布者身份和信任策略。

# WASM 插件架构重构（方案二）

核心原则：WASM 插件运行时是 App 始终启用的基础能力，不设置 `host` feature；App 只加载插件，`tiangong-plugin-runtime` 管通用协议、制品发现、连接和生命周期，WASM 定义业务操作，sidecar 承载完整业务。

## 阶段一：协议和基础结构

- [x] 更新 `PLAN.md`，补充 WASM 与 sidecar 架构规划。
- [x] 更新 `TODO.md`，按独立任务拆分重构工作。
- [x] 在 `tiangong-plugin-runtime` 内建立通用 sidecar 请求/响应、错误、握手和传输协议，不包含 Memory 业务类型。
- [x] 给 WIT 增加版本号（`package tiangong:plugin@0.1.0`）。
- [x] 增加通用 sidecar WIT interface（`sidecar` 或 `companion`），含请求/响应结构和通用错误类型。
- [x] 删除 WIT 中的 `memory-store` interface 和 world 中的 `import memory-store`。
- [x] 统一 WIT 来源为单一事实源，消除两份副本。
- [x] 定义插件制品清单格式（插件 ID、版本、WASM 路径和 sidecar 启动信息），由运行时自动发现。
- [x] 在 Memory 插件目录恢复私有协议 crate，集中维护业务操作和数据结构。

### 阶段一完成标准

- Host 能识别带 sidecar 的插件。
- Memory WASM 只调用通用 WIT，协议封装和响应校验由运行时完成。
- 通用 WIT 中不再出现 Memory。

## 阶段二：通用 sidecar 管理

- [x] 实现通用插件 sidecar 管理器，不依赖 Memory crate，也不提供 Memory 专用便捷函数。
- [ ] 实现插件 sidecar 安装（分平台选择、完整性验证、权限设置）。
- [x] 实现启动和真实健康检查（协议握手，不只检查 endpoint 文件）。
- [x] 实现认证（每次启动生成短期凭据）。
- [x] 实现请求转发（通用 JSON 负载、请求 ID 匹配）。
- [ ] 实现超时、大小限制、并发限制。
- [ ] 实现崩溃检测和有限重启（退避策略、连续失败禁用）。
- [ ] 实现日志收集（写入插件私有目录、轮转、脱敏）。

### 阶段二完成标准

- App 通过通用插件加载触发配套 sidecar 启动，入口代码不知道 Memory 制品路径和协议。
- WASM 能通过通用接口完成一次私有协议调用。
- Host 不解析 Memory 请求内容。
- sidecar 崩溃后能明确返回错误或恢复。

## 阶段三：Host 模型代理

- [ ] 定义模型代理协议（文本生成、结构化输出、Embedding、Rerank）。
- [ ] 实现文本生成和结构化输出。
- [ ] 实现 Embedding 和 Rerank。
- [ ] 增加短期认证（每次启动重新生成，只允许当前 sidecar 使用）。
- [ ] 增加权限和配额（按插件限制能力、模型、并发、频率、费用）。
- [ ] 增加用量统计与脱敏日志。

### 阶段三完成标准

- sidecar 不读取真实模型密钥。
- sidecar 不直接访问外部模型。
- 模型请求统一经过 Host。
- Host 可以识别请求来自哪个插件。

## 阶段四：Memory 迁移

- [x] Memory WASM 改用通用 sidecar 接口，业务操作名和负载只存在于 Memory 插件与 sidecar。
- [x] Memory WASM 与 sidecar 改为共同依赖插件私有协议，不再分别维护操作字符串与负载结构。
- [ ] Memory sidecar 改用 Host 模型代理替换直接模型调用。
- [x] 修复会话和工作区上下文（显式携带 session_id、workspace_id、turn_id）。
- [x] 修复 Micro/Meso/Meta 反刍请求，恢复原版完整轮次提取和每 10 轮整理。
- [x] 收尾钩子（`on_turn_finished` / `on_session_ended`）改为 fire-and-forget 投递，不保存后台线程句柄，记忆反刍通知不再阻塞回复收尾关键路径或持续占用已结束线程资源。
- [x] 保持原有 `~/.tiangong/memory` 数据、配置、索引和注入目录，不迁移到插件目录。
- [x] Memory UI 改走新链路。
- [x] 配置只保存逻辑模型键，不保存 API Key / Base URL。
- [x] 验证召回、写入、反刍和管理页面使用的插件协议链路。

### 阶段四完成标准

- Memory 工具能正常召回。
- 每轮反刍真实执行。
- 会话结束整理真实执行。
- 不同工作区不会串数据。
- Memory 页面 CRUD 正常。

## 阶段五：清理旧链路

- [x] 删除 Memory Host 专用接口、独立 Memory 协议 crate 和 `HostState` 中的 Memory 依赖。
- [x] 删除直接 Memory Handle 注入和进程内 Actor 降级。
- [x] 删除三入口中的 Memory 专用加载、启动和进程内降级逻辑。
- [x] 删除重复 WIT。
- [ ] 将 sidecar 直接模型调用迁移到 Host 模型代理。
- [ ] 删除旧配置读取路径和完整 `CoreConfig` / Session JSON 注入。

### 阶段五完成标准

- 全仓不存在旧调用路径，App、CLI、Server 入口不存在 `load_memory_*` 或 `memory_sidecar_*` 调用。
- Desktop、CLI、Server 都走统一插件链路。
- Memory 相关检查、三入口编译和依赖检查通过。

## 当前回归修复：Memory 数据恢复与 MCP 插件页面

- [x] App 层通过插件运行时通用注入 `storage_root`，Memory 与 MCP 从注入根目录读取业务数据和配置。
- [x] Memory sidecar 启动时识别改造期间误写入插件私有目录的旧数据，并在既有目录为空时恢复到 `~/.tiangong/memory`。
- [x] MCP WASM 提供完整的服务器管理页面，支持查看、刷新、添加、编辑、启停和删除。
- [x] 删除宿主设置页中的旧 MCP 入口与旧管理组件，仅保留插件动态入口。
- [x] 重新构建 Memory、MCP 插件制品与前端，并验证真实数据和页面流程。
- [x] App 提供通用插件页面 mask hook，允许插件按 channel 显示或隐藏宿主遮罩并传入颜色，MCP modal 使用同色遮罩覆盖宿主区域。
- [x] 动态插件设置页使用完整 flex 布局并隐藏 App 侧溢出，页面滚动由插件 UI 内部处理。
- [x] 修复 Memory 插件页面的刷新、缩放、适应画布、分页和关闭按钮图标不显示。

### 当前回归修复完成标准

- 原有 Memory 数据无需手工搬运即可重新显示，恢复过程不覆盖已有有效数据。
- App 使用自定义存储根目录时，Memory 与 MCP 不再回退读取用户主目录下的另一份数据和配置。
- 设置页只显示插件提供的 MCP 管理入口，页面功能与迁移前一致。
- Memory、MCP、桌面端和前端检查通过，实际页面无空白或重复入口。
- 插件弹窗打开时宿主与插件区域的遮罩连续同色，关闭弹窗、切换页面或卸载 iframe 后不残留宿主遮罩。
- 动态插件页占满设置内容区，App 侧不出现滚动条，插件长内容仍可在 iframe 内滚动。
- Memory 页面所有图标按钮显示清晰，且不影响图谱中的 SVG 节点和连线。

# Coding 专用 WASM 插件 PoC

## 目标与边界

- [x] 新增 Coding WASM + sidecar 插件，通过提示词编排现有 fs、index、command、terminal 等能力，并补充项目上下文、开发前检查、进度记录和交付审查能力。
- [x] 不重复实现通用文件、检索、命令和终端工具，也不在插件内建立第二套 Agent Loop。
- [x] 接收当前工作区和完全信任状态；sidecar 只提供 Coding 专用的结构化聚合能力。
- [x] 接入统一插件构建、部署和 OSS 制品生成流程。

## 当前任务：插件骨架、提示词与 sidecar 增强

- [x] 创建 Coding 私有协议、WASM Component、sidecar 与 `plugin.json`。
- [x] 注入项目约定发现、代码探索、最小改动、真实验证、失败修复和简明汇报规则，不预设固定规则文件名、技术栈或包管理方式。
- [x] 明确现有检索、文件、命令和终端能力的调用顺序及安全边界。
- [x] 提供项目上下文、开发前检查、进度记录和交付审查工具，不重复暴露已有原子能力。
- [x] 在 xtask 注册 `build-plugin coding`。
- [x] 构建 Coding 完整插件，校验清单、WASM、sidecar 和提示词内容。

## 完成标准

- `cargo run -p xtask -- build-plugin coding` 能完成协议检查、WASM 与 sidecar 构建、部署和 OSS 制品生成。
- 插件可被现有运行时加载，描述符、sidecar 握手与清单 ID、版本一致。
- 启用插件后能注入 Coding 工作流并提供 Coding 专用聚合工具，停用后不再注入或暴露工具。
- 不改变宿主 Agent Loop，不重复暴露已有原子工具。

## 已完成修复：流式工具调用与 Coding 交付检查

- [x] OpenAI 兼容流式事件完整保留工具调用序号和结束原因。
- [x] 并行工具调用按序号累计名称、ID 和全部参数分片，重复开始事件不覆盖已有内容。
- [x] 参数截断或 JSON 解析失败不执行工具，写入失败审计并只触发一次针对性修正。
- [x] Coding 交付检查自动合并上游基线至当前分支的已提交改动和工作区改动，不再要求完整文件列表。
- [x] Coding 项目上下文与开发前检查减少重复内容，旧任务进度不再串入新任务。
- [x] 重新构建并部署 Coding 插件，完成 LLM 定向检查、Core 检查、插件完整构建和代表性 Git 场景验证。

### 本次完成标准

- 并行、乱序、重复开始和嵌套 JSON 参数均能按原调用顺序完整还原。
- 长参数被模型截断时不会进入真实工具执行，并能在会话、界面和审计记录中看到明确失败。
- 当前工作区干净但分支存在已提交改动时，Coding 交付检查仍能列出实际改动；普通短调用无需提交文件列表。
- 新任务不会收到同一工作区旧任务的进度记录，插件提示和工具结果不重复灌入无关上下文。

## 已完成修正：收窄 OpenAI 工具参数恢复范围

- [x] 撤回 Core、DeepSeek 底层库、Anthropic 和 DeepSeek 协议适配层的连带修改，保持既有执行逻辑。
- [x] 仅在 `tiangong-llm` 的 OpenAI Chat Completions 链路校验工具名称和参数是否符合本次 `tools` 定义。
- [x] 并行调用中保留有效调用、剔除无效调用，仅携带失败调用涉及的工具定义自动修正一次。
- [x] 合并并去重首次有效调用与修正后的有效调用，确保无效调用不会进入 Agent。
- [x] 完成 LLM、Core、DeepSeek 和 Coding 插件验证。

### 本次完成标准

- Core 与 `tiangong-deepseek` 相对 Coding 插件初始提交没有新增改动。
- OpenAI 单个或并行工具调用只要名称、必填参数或参数类型不符合 schema，就会在 LLM 层被剔除并定向修正一次。
- 并行调用中的有效项不会因其他调用失败而丢失或重复执行，修正后返回给 Agent 的调用全部通过本次 `tools` 定义校验。
- DeepSeek 和 Anthropic 请求次数及工具调用行为保持不变。

## 当前性能修复：恢复 WASM 编译缓存

- [x] 恢复 App 级共享 WASM 编译环境，并缓存每个插件的编译结果。
- [x] 新建会话 Core 时只创建独立插件实例，不再重复编译 WASM 或读取插件描述。
- [x] 热加载时只编译一次新版本，并让设置页和全部存活 Core 复用同一编译结果。
- [x] 通过插件运行时格式、编译、严格检查和现有验证。

### 本次完成标准

- 同一插件只在预加载或热加载时编译一次，连续切换会话不再触发重复编译。
- 热加载失败继续保留旧实例，成功后新建会话与存活会话均使用新编译结果。
- 插件能力、资源限制、sidecar 路由和设置页行为保持不变。

## 当前修复：Coding 长任务自主完成

- [x] 明确未完成、验证失败和审查未通过时必须继续处理，不向用户交付阶段性未完成报告。
- [x] 阶段总结发现仍有可执行工作时使用运行时续作标记，不要求用户发送“继续”。
- [x] 仅在缺少用户决定、授权、凭据或无法替代的外部条件时暂停，并重新构建 Coding 插件验证提示内容。

### 本次完成标准

- 长任务达到阶段总结边界时，未完成工作自动回到工具执行阶段。
- 编译失败、测试失败、待补文档、待验证、工作量或已执行轮次均不被当作外部阻碍。
- 只有任务完成并验证通过后才向用户汇报结果；确需外部介入时说明具体缺口。

## 当前回归修复：终端长命令与失效恢复

- [x] 非交互命令仅在 Agent 明确设置时才启用超时；未设置时持续到完成或中断，不再使用固定默认值或额外 180 秒提前取消。
- [x] 用户键盘输入和 `Ctrl+C` 不经过长命令串行队列，可立即写入当前 PTY。
- [x] PTY 读取结束或写入失败后准确标记终端失效，下一次输入或命令自动重建。
- [x] 隔离会退出或污染长期 shell 的控制语句，并正确回收退出的 PTY 子进程。
- [x] 非交互命令改由临时脚本承载，避免前台程序吞掉完成标记或 zsh 拆散内部包装；带超时命令实际结束后立即返回。
- [x] 并行命令选择终端时先原子占用 Tab，繁忙时使用其他空闲 Tab 或创建新 Tab。
- [x] Agent 执行时只在终端显示完整原命令和真实输出，内部脚本调用不会泄漏，并同步保留终端日志。
- [x] 使用真实 zsh PTY 验证命令完成、连续复用同一 Tab、活动状态释放和内部输出过滤，并完成前端正式构建。

### 本次完成标准

- Agent 仅在存在明确时间边界时按实际需要设置超时；未设置时终端不会自行提前取消命令。
- Agent 命令运行期间，用户可以立即使用 `Ctrl+C` 中断，命令结果能识别用户介入。
- 带 `set -e` 的失败或中断脚本不会带走长期 shell，后续手动和 Agent 命令均可继续执行。
- PTY 意外退出后不再显示为存活或被当作空闲终端复用，恢复后没有未回收子进程。
- 带超时命令在实际结束后立即返回；同轮并行命令不会排进同一个终端或互相破坏完成标记。
- Agent 命令完成后对应 Tab 恢复空闲并可继续复用；终端中不出现内部包装、语法错误或残缺命令。

## 当前任务：跨平台后台命令规则

- [x] 根据 Rust 编译目标只注入当前系统适用的后台命令 Prompt。
- [x] macOS/Linux 使用 `nohup`、后台运行和显式日志重定向；Windows 使用 PowerShell `Start-Process`。
- [x] 后台命令规则要求返回进程编号和日志路径，并在宣告服务可用前检查进程、日志或健康状态。
- [x] 增加平台 Prompt 单元测试并完成终端插件编译与严格检查。

### 本次完成标准

- 构建产物只包含当前系统的后台命令指导，不会让 Agent 混用 Unix 与 Windows 命令。
- 后台服务启动后工具调用及时返回，Agent 能从启动结果取得进程编号和日志位置并按需检查。
- 平台 Prompt 有单元测试覆盖，终端插件测试和严格检查通过。

## 当前修复：交互任务生命周期与终端增量注入

- [x] interactive 任务或 PTY 结束后，输入、状态查询、最近输出、屏幕上报和 terminal_send 不自动重建终端。
- [x] 用户主动重置或 Agent 明确执行新命令时才创建新 PTY，且不恢复已结束的交互任务。
- [x] terminal_data 保留每个 Tab 的完整元状态，只携带上次成功注入后新增的输出。
- [x] 为增量输出携带单调游标，注入成功后提交游标，注入失败时下一轮继续补发。
- [x] Core 实例恢复时把已有终端的输出游标移动到当前末尾，不重复注入恢复前历史。
- [x] Agent 未指定 Tab 时，按创建时间和 ID 顺序优先选择最靠前的存活空闲终端；全部繁忙时新建 Tab。
- [x] 增加生命周期和增量游标验证，通过终端插件测试、严格检查、Core 检查与前端正式构建。

### 本次完成标准

- 交互任务退出后不会因任何隐式读取或输入重新启动 Shell，调用方能看到明确的不可用状态。
- 显式新命令和用户重置仍能恢复终端，但只启动新的 Shell/任务。
- 连续两轮无新增终端输出时不重复注入旧内容；有新增输出时仅注入新增部分且游标连续。
- 注入通道失败不会提前消费输出，下一次注入仍能取得未交付内容。

# 后续插件 WASM 化排期

Memory 作为「重型、带 sidecar」的样板已迁移完成。按「从难到易」推进，并明确区分哪些插件可改造、哪些必须保留原生。

判断依据是每个插件的耦合点：wasm 沙箱无文件系统/网络/子进程/mmap/系统句柄；所有重能力下沉到独立原生 sidecar 进程，wasm 只做无状态桥接；现有 host import 只有 `clock` / `sidecar.invoke` / `feedback.emit-stream-event` 三项。

## 一、必须保留原生（3 个）

这些插件持有 host 进程内的 GUI/系统句柄，或就是 host 运行时自身，无法下沉到 sidecar，也无 headless 替代方案。

| 插件 | 卡死点 |
|---|---|
| **agent-team** | 在 turn 内反向构造完整 `TiangongCore` 跑子 Agent，外加每个子 Agent 独占一个 OS 线程。wasm 由 host 加载，不能反向实例化 host Core，是 host 运行时自身的能力。 |
| **terminal** | 强依赖 PTY 系统句柄（`portable-pty`/`vte`/`libc`），且输出经 Tauri 事件直连前端 xterm 面板。PTY 搬不进沙箱，回显链路离不开 host 界面。 |
| **browser** | 整个插件就是操作内嵌 WebView 标签页，外加 25 个前端命令、Tauri `unstable` 句柄。GUI 句柄无法跨进程，headless sidecar 接管不了。 |

## 二、可 WASM 化，需配原生 sidecar（7 个，从难到易）

照搬 Memory 的「wasm 桥接 + 原生 sidecar」模板，重能力整体下沉到独立进程。sidecar 的单例淘汰（已有实例则退出）由通用运行库 `tiangong-plugin-sidecar` 提供，app 只按 `plugin.json` 启动 sidecar、不参与其单例判定。

| 顺序 | 插件 | 下沉内容 | 关键设计点 |
|---|---|---|---|
| 1 | ~~**mcp** #325~~ ✅ | rmcp 子进程 transport + HTTP + 后台探测线程 + capability cache | 已完成。三子 crate（protocol/sidecar/wasm）+ 四入口管理 API 经 invoke_sidecar + 原生 crate 已删除。 |
| 2 | **fetch** #326 | 网络栈（reqwest 阻塞式）+ HTML 解析（scraper）+ DNS + 落盘 | reqwest 阻塞客户端独占线程与 runtime，是硬卡点；逻辑封闭、无 GUI 回路，改造相对干净。 |
| 3 | ~~**index** #327~~ ✅ | tantivy（mmap）+ notify 文件监听 + 后台扫描 | 已完成。三子 crate（protocol/sidecar/wasm），照 MCP 模板：核心实现 + 单例淘汰（通用运行库）内置 sidecar；wasm 桥接 index_search/search_code 工具与生命周期钩子；三入口改走 load_installed_plugins，原生 crate 与 5 个 GUI 索引管理命令已删除；工作区自动刷新按路径、修改时间和大小增量处理，用户主动重建仍执行全量重建。 |
| 4 | ~~**scheduler** #328~~ ✅ | cron 调度 + JobStore + 任务去重 + 执行记录 | 已完成。三子 crate（protocol/sidecar/wasm），照 Index 模板：sidecar 长驻跑 silent cron 调度 + JobStore + 任务去重，到点经 HTTP 调本机 server `POST /api/v1/messages`（`connector=server-api` + `channel_id=session_id` 直投）投递消息；host 启动 sidecar 时注入 `TIANGONG_SERVER_URL`/`TIANGONG_SERVER_TOKEN` 两个新标准 env（扩展通用 `SidecarConfig`）；三入口与 Tauri/REST 写操作改走 `invoke_sidecar`，进程内 `SchedulerContext`/`restore_cron_jobs`/`DesktopSchedulerContext`/`ServerSchedulerContext` 全部移除，`tiangong-scheduler` 核心库瘦身为只保留 model/store/validate_cron_schedule。 |
| 5 | **task** #329 | 子进程 spawn + 任务注册表 | 任务列表当前被 GUI/Tauri 直接读取，sidecar 化后需解决注册表跨边界一致性。 |
| 6 | **fs** #330 | `std::fs` 全套 + 进程级全局锁表 | 走 sidecar。文件读写虽可由 WASI 承接，但走 sidecar 的真正理由是**锁表需跨 wasm 实例全局共享**（主/子 Agent 写同一文件互斥）：wasm 实例间内存隔离，锁表只能落在所有实例共享的 sidecar 进程内；路径解析（动态工作区 + FullTrust 越界）随之一起下沉，避免为 fs 专用定制 WIT host import。 |
| 7 | **command** #331 | 子进程 spawn（tokio process） | 最简单的 sidecar：校验与拆分进 wasm，仅 spawn 下沉，无 GUI 纠葛。 |

## 三、可 WASM 化，轻量（6 个，待 Host 模型代理）

这组插件层是纯编排，不直接持密钥（统一经 `tiangong-llm` + `tiangong-config`，真正网络调用在 core 侧）。需等「方案二·阶段三 Host 模型代理」落地，由 host 代发模型请求、解析密钥。

| 顺序 | 插件 | 卡点 | 前置 |
|---|---|---|---|
| 8 | **skill** #332 ✅ | 文件读写 + 环境变量 + 管理 API 被直调 + exec_env 回传 | 配小 sidecar，管理接口重新经插件页面暴露（不依赖模型代理）。已完成 WASM 化（wasm + sidecar + protocol 三子 crate），管理操作经插件设置页，exec_env 暂置空 |
| 9 | **analyze-attachment** #333 | 多模态模型调用 | 等 Host 模型代理 |
| 10 | **generate-image** #334 | 模型 + 图片归档落盘 | 等 Host 模型代理 + 媒体归档接口 |
| 11 | **text-to-speech** #336 | 模型 + 音频写入 | 等 Host 模型代理 + 媒体写入接口 |
| 12 | **speech-to-text** #337 | 模型 + 音频读取 | 等 Host 模型代理 + 媒体读取接口 |
| 13 | **generate-video** #335 | 模型 | 等 Host 模型代理（无本地落盘，最简单的媒体类） |

## 四、最易，立即可做（1 个）

| 顺序 | 插件 | 说明 |
|---|---|---|
| 14 | **prompt** #338 | 纯字符串注入 + config 缓存，零副作用，现有 WIT 全覆盖。与 Memory 形成「轻/重」两端对照。 |

## 已完成（3 个）

| 插件 | 模式 |
|---|---|
| **memory** | wasm 桥接 + 原生 sidecar（重型样板） |
| **mcp** | wasm 桥接 + 原生 sidecar（#325，四类耦合，工程量最大） |
| **index** | wasm 桥接 + 原生 sidecar（#327，tantivy mmap + 后台扫描 + rg/grep） |

## 共同约定

- 每个重型插件按 Memory 模板建立私有协议 crate（业务操作名 + 请求/响应），wasm 与 sidecar 共同依赖，App 不依赖。
- sidecar 的单例淘汰（已有实例则退出，经 endpoint 文件 + TCP 可达性探测判定）由通用运行库 `tiangong-plugin-sidecar` 提供，各 sidecar 引入该库并实现 `SidecarService` trait，自身仍负责业务服务。app 只按 `plugin.json` 启动 sidecar、不参与其单例判定。memory 因同时服务 in-process 场景保留自有复杂单例机制，待后续统一。

## 完成标准

- 必须原生的 3 个插件长期保留，不纳入 WASM 化排期。
- 每个 sidecar 插件迁移后，三入口均能加载该 WASM 制品，原有能力不回退，重能力下沉到独立进程。
- Index 工作区索引自动刷新只处理新增、修改和删除文件；旧 schema 首次打开时自动全量校准，旧索引目录恢复时不覆盖现有有效数据。
- Index 生命周期调用失败可在宿主日志中定位但不阻断对话；Core 结束会话前必须等待活跃 turn 完成，确保结束钩子只处理最终会话数据。
- 轻量插件等 Host 模型代理落地后再迁，密钥由 host 解析，不进 wasm/sidecar。
- 每完成一个插件，更新本节状态并补充对应模板说明。

# #250 移动端控制 / 通讯网关 TODO

## Bot MCP 主动推送

### 方案设计（已完成）

- [x] 核对现有 Bot、MCP、Server API 和定时任务链路。
- [x] 明确 MCP 只承担出站发送工具，触发仍由定时任务、Webhook 或用户任务负责。
- [x] 完成 stdio 运行方式、普通 MCP 快速注册、目标授权、凭证隔离和幂等处理设计。
- [x] 明确飞书、QQ、微信的能力差异与分阶段接入范围。
- [x] 形成 `docs/bot-mcp-proactive-push-design.md` 设计方案。

### 平台能力验证（进行中）

- [x] 验证飞书脱离原消息编号向已知 `chat_id` 发送文本、图片和文件。
- [ ] 验证 QQ 私聊和群聊主动消息请求、权限、配额与时间窗口。
- [ ] 验证微信 `context_token` 在私聊、群聊和不同时间间隔下的复用范围。
- [x] 在验证长期发送能力前，将 QQ 和微信固定为 `reply_window`，不宣称 `ready`。

### 通用协议与飞书闭环（已完成）

- [x] 扩展 Bot 能力描述和完整描述缓存，保持旧 Bot 兼容。
- [x] 增加推送目标发现、持久化和用户授权管理。
- [x] 增加 `bot --mcp generate` 配置生成、`bot --mcp` stdio 模式及文本发送工具。
- [x] 增加飞书 MCP 图片和文件上传、发送工具，并限制可读取的本地目录和文件大小。
- [x] 在 Desktop 中按能力接入普通 MCP 注册能力，后续由 Bot 生命周期自动调用。
- [x] 接入投递幂等记录和明确失败状态，避免记录消息正文与凭证。
- [x] 完成飞书真实文本、图片、文件发送和重复调用防重验证。

### 微信与 QQ 受限 MCP（已完成）

- [x] 为微信和 QQ 增加 MCP 能力声明、配置生成、stdio 服务和目标授权管理。
- [x] 保存微信最近 `context_token`，在回复窗口内发送文本、图片和文件。
- [x] 保存 QQ 最近 `msg_id`，在回复窗口内发送文本、图片和文件。
- [x] MCP 子进程从现有 Bot 实例读取扫码或手工凭证，不把凭证写入 MCP 配置。
- [x] 修复 `reply_window` 目标误判为不可用，并接入 Bot 自维护授权清单与删除能力。
- [x] 对本地媒体执行目录、普通文件、格式和大小校验，并复用幂等投递记录。
- [x] 完成两个 Bot 的格式、检查、严格检查、构建和本地制品打包。

### Bot 多目标清单与 MCP 自动生命周期（已完成）

- [x] Bot 自动维护所有已发现的推送目标，移动端会话主动发来消息时自动授权对应目标。
- [x] 发送工具保留目标编号，Agent 可以从 Bot 清单中选择不同目标分别发送。
- [x] 保留 `bot_register_mcp`，前端启动支持 MCP 的 Bot 后自动注册，应用重启恢复时再次确认；旧 Bot 保持原行为。
- [x] 用户停止 Bot 或删除 Bot 配置时自动注销对应 MCP，并拒绝覆盖或删除同名的其他 MCP。
- [x] 移除移动端控制页的手工 MCP 注册按钮和授权开关，只保留授权清单及删除入口。
- [x] 完成三端 Bot、Desktop 和前端的格式、检查、构建及关键流程验证。

### Bot CI 检查（已完成）

- [x] CI 能自动发现并识别 `bots/*/Cargo.toml` 下的独立 Bot 工程改动。
- [x] 每个受影响 Bot 独立执行格式、编译、严格检查和现有测试。
- [x] CI 配置变更时三个 Bot 全部执行，未改动的 Bot 不产生任务。

### Bot 0.2.0 版本升级（已完成）

- [x] 飞书、微信和 QQ Bot 的制品版本与锁定信息统一升级为 `0.2.0`。
- [x] 微信协议请求中的 Bot 客户端版本同步升级为 `0.2.0`。
- [x] 三个 Bot 均通过格式、编译、严格检查和现有测试。
- [x] 推送三个 `v0.2.0` 独立发布标签，完成四平台制品、校验文件和公网索引发布。
- [x] 使用本机三个 `0.1.0` Bot 验证应用升级链路，升级后版本、校验值和 MCP 能力均正确。

### Bot 更新入口交互（已完成）

- [x] 进入移动端控制页时自动判断已安装 Bot 是否存在更新，不再提供手工“检查更新”按钮。
- [x] 有更新时直接显示带文字的“更新”按钮，无更新时不显示更新入口。
- [x] 更新成功后刷新本地版本并隐藏更新按钮，更新失败时保留入口以便重试。
- [x] 更新开始前停止 Bot，更新结束后恢复原有运行状态和自动运行设置。
- [x] Bot 管理页及相关弹窗的操作按钮统一使用紧凑尺寸，并显示中文动作名称。
- [x] 通过前端正式构建，并使用 `0.1.0 → 0.2.0` 数据验证桌面和窄屏下更新按钮的显示与消失。

### Bot 长任务等待修复（已完成）

- [x] 飞书、微信和 QQ Bot 转发消息到天工时不再受 120 秒总时限限制，持续等待任务完成。
- [x] Bot 平台接口、扫码、上传和 MCP 请求继续保留原有超时，避免外部请求无限等待。
- [x] 三个 Bot 均通过格式、编译、严格检查和现有测试。
- [x] 使用延迟 125 秒的本地 Mock 接口完成三端长任务回归验证。

### Bot 长任务等待修复完成标准

- 天工任务执行超过 120 秒时，Bot 不会提前回复“处理消息时出现了错误”。
- 天工返回成功或明确错误后，Bot 才结束本次消息处理。
- 飞书、微信和 QQ 平台自身的网络超时行为保持不变。

### Bot 0.2.1 修复版本发布（已完成）

- [x] 飞书、微信和 QQ Bot 的制品版本与锁定信息统一升级为 `0.2.1`。
- [x] 三个 Bot 均通过格式、编译、严格检查、现有测试和正式构建。
- [x] 通过独立发布标签完成三个 Bot 的四平台制品、校验文件和公网索引发布。
- [x] 核对三个 GitHub Release 和 OSS 索引中的版本、下载地址与校验值。

### 完成标准

- 旧 Bot 行为不变；声明能力的 Bot 配置或启动成功后自动提供 MCP 工具。
- Bot 自动授权曾经主动联系过它的会话，应用可展示或删除授权记录，Agent 可按授权清单选择多个不同目标。
- 飞书移动端能收到定时任务触发的真实文本、图片和文件，同一幂等键不会重复发送。
- 用户停止 Bot 或删除 Bot 配置后，对应 MCP 自动注销；再次启动时自动恢复注册。
- 凭证和平台上下文令牌不进入 MCP 配置、工具结果或日志。
- QQ、微信只显示为回复窗口内可尝试发送；真实验证长期能力前不得显示为 `ready`。

# #270 QQ Bot 接入

## QQ Bot 消息收发修复（已完成）

- [x] 使用 QQ 私聊和群聊事件对应的订阅范围建立连接。
- [x] 记录收到的事件类型，便于确认消息是否进入 Bot，且不记录消息正文。
- [x] 通过 Rust 格式、测试、严格检查和正式构建，并重新部署本地 QQ Bot。
- [x] 按 QQ 官方消息结构读取私聊用户标识，避免回复地址缺少收件人。
- [x] 私聊用户标识缺失时停止处理，并记录可诊断日志。
- [x] 记录 QQ 文本回复发送成功状态。
- [x] 重新通过 Rust 检查和正式构建，并部署本地 QQ Bot。
- [x] 本地图片回复改用 QQ 官方分片上传流程，并通过真实图片上传与发送验证。

## QQ Bot 直接扫码配置（已完成）

- [x] 通过 QQ 官方绑定服务创建一次性扫码任务，并展示任务专属二维码。
- [x] 轮询 QQ 官方绑定结果，正确处理等待、成功、过期和临时网络失败。
- [x] 扫码成功后由 QQ bot 解密并保存 AppID、AppSecret，主程序不接收或回填凭证。
- [x] 保留 AppID、AppSecret 手工配置入口作为备用方式。
- [x] 通过 Rust 格式、检查、严格检查、现有测试及真实接口等待状态验证。
- [x] 兼容 QQ access token 有效期的实际返回格式，并避免解析错误日志泄露访问凭证。
- [x] 兼容不带消息内容的 QQ 心跳确认，避免正常心跳被误报为解析异常。

### QQ Bot 直接扫码配置完成标准

- 配置 QQ bot 时可以直接生成有效二维码，不必先填写 AppID 或 AppSecret。
- 用户可在手机 QQ 中选择已有机器人或创建机器人，确认后自动完成配置。
- 扫码成功后凭证只由 QQ bot 保存，保存 bot 条目后可以直接启动。
- 二维码过期、接口暂时失败或用户取消后不会保存无效配置，仍可重新扫码。
- 手工配置已有 QQ 机器人的方式继续可用。

## 微信 Bot（已完成）

- [x] 微信扫码入口可生成有效二维码，未扫码时稳定返回等待状态，扫码成功后由 bot 自行保存凭证。
- [x] 按当前 iLink 协议接收文本与图片消息，并把天工的文本回复发回原微信会话。
- [x] 同一私聊或群聊持续复用同一天工会话；外部消息编号用于防止重试时重复执行。
- [x] 修复外部通道会话关联未落盘，导致微信连续消息反复创建新会话的问题。
- [x] 图片下载地址、密钥格式和解密流程与当前 iLink 协议一致，图文消息不丢失图片。
- [x] 微信 bot 可独立构建和发布，发布流程能从实际构建目录取得四个平台制品。
- [x] Rust 格式、检查、严格检查、现有验证、发布构建及真实扫码等待状态验证全部通过。
- [x] 天工回复中的本地媒体路径由 Bot 读取并上传到微信，移动端收到真实图片或文件，不显示本机路径。
- [x] 只允许发送天工媒体目录或当前会话工作区内的真实文件，拒绝回复中指向其他本机位置的路径。
- [x] 天工 Connector 响应保留同一回复中的全部结构化附件，微信 Bot 按顺序发送文本和附件。
- [x] 微信出站文件回传通过格式、检查、严格检查、现有验证及本地制品构建。

### 微信 Bot 完成标准

- 配置页可以生成微信二维码；普通扫码完成后无需手工填写 Token 即可保存并启动 bot。
- 同一微信会话连续发送多条消息时保留上下文，重复投递不会重复触发天工任务。
- 文本、纯图片和图文消息都能进入天工；微信能收到对应的文本回复，以及天工回复中引用的真实图片或文件。
- 网络或处理失败时不提前丢弃轮询进度，恢复后可以重试原消息。
- 本次不增加语音、文件、视频、主动消息和手机配对码输入界面。

## 飞书图片转发修复（已完成）

- [x] 单图消息不再因正文为空被丢弃，使用结构化图片内容转发。
- [x] 纯图片富文本保留全部图片，第一张作为正文，其余图片按原顺序进入媒体列表。
- [x] 图文富文本同时保留文字和全部图片。
- [x] 图片下载失败、天工调用失败、空消息和不支持类型均清理接收标记，只有完整成功后添加完成标记。
- [x] 同一通道重复收到相同飞书消息时不重复转发，转发前失败仍允许后续重试。
- [x] 通过飞书 Bot 格式、检查、测试及前端构建。

## Bot 独立发布索引修复（已完成）

- [x] 将失效的 Intel Mac 构建环境更新为当前可用环境，确保四个平台制品都能完成发布。
- [x] 飞书、微信和 QQ 发布流程分别写入自己的 OSS 索引对象，不再并发读写同一个根索引。
- [x] 主程序从远端目录发现并合并各 Bot 索引，新增 Bot 不需要修改或重新发布主程序。
- [x] 使用三个 `v0.1.0` tag 完成真实发布，并验证更新列表、当前平台下载和 SHA-256 校验。
- [x] PR #275 审查并合并后，确认目录文件已由主分支流程自动发布到 OSS。

### Bot 独立发布索引修复完成标准

- 三个 Bot 同时发布时不会互相覆盖索引，也不需要共享写锁或依赖发布顺序。
- 每个 GitHub Release 包含四个平台制品、对应校验文件和本 Bot 的索引文件。
- OSS 上三个独立索引都可读取，主程序合并后正好得到飞书、微信和 QQ 三个 Bot。
- 当前平台三个制品都可从索引地址下载，且实际 SHA-256 与索引完全一致。

## 第三方 Bot 目录接入 CI（已完成）

- [x] 修改根目录的 PR 自动校验目录格式、HTTPS 地址和重复地址。
- [x] 自动读取每个下级索引，校验 Bot ID、版本、制品地址和 SHA-256 格式。
- [x] 拒绝跨索引重复 Bot ID，避免第三方覆盖已有 Bot。
- [x] PR 校验不读取 OSS 密钥，只有主分支更新或手工触发时才执行上传。
- [x] 合并后继续自动发布并从公网核对根目录内容。

### 第三方 Bot 目录接入 CI 完成标准

- 第三方只需提交修改 `bots/index-catalog.json` 的 PR，即可获得明确的索引检查结果。
- 无效、不可访问或与现有 Bot ID 冲突的索引不能通过检查。
- PR 不具备 OSS 写入权限；合并到主分支后才会更新线上根目录。

## 本地自有 Bot 接入（已完成）

- [x] 本地 Bot 只有可执行文件时，配置页自动调用 `--describe` 并缓存配置定义。
- [x] 拒绝不支持的描述版本，以及与目录名不一致的 Bot ID。
- [x] 本地 Bot 不需要远端索引、PR 或手写 `schema.json`、`version.json`。
- [x] 在 Bot README 中补全开发协议、官方目录贡献和仅本地使用两种方式。
- [x] 使用代表性本地 Bot 完成发现、描述、配置和启动前检查验证。

### 本地自有 Bot 接入完成标准

- 把可执行文件放到 `~/.tiangong/bots/<bot-id>/bot`（Windows 为 `bot.exe`）后，刷新页面即可看到并配置。
- 配置字段来自该程序的 `--describe` 输出，启动时按字段声明注入环境变量。
- 本地方式不会读取或修改官方根目录，也不会把 Bot 或配置上传到外部。

## 在线 Bot 安装界面（已完成）

- [x] 移动端控制页面同时加载线上目录、本地制品和已配置 Bot，不再只扫描本地目录。
- [x] 未安装的线上 Bot 展示名称、说明和版本，并可直接下载安装。
- [x] 安装成功后直接进入配置流程，已有本地 Bot 和离线使用方式保持可用。
- [x] 线上目录不可达时明确展示错误并允许重试，不影响本地 Bot 的管理。
- [x] 完成前端正式构建、Rust 检查和真实界面流程验证。

### 在线 Bot 安装界面完成标准

- 全新安装且本地没有 Bot 时，页面可以看到线上可安装的飞书、微信和 QQ Bot。
- 点击安装后能下载并校验当前平台制品，随后可以完成配置。
- 已安装 Bot 不重复出现在在线列表；线上不可达时仍可操作本地 Bot。

## Bot ID 安全校验（已完成）

- [x] 增加统一的 Bot ID 类型，限制为首位小写字母、后续小写字母或数字、总长 1～64 位，并拒绝 Windows 保留名称。
- [x] 配置加载、桌面命令、管理、扫码、日志、健康查询、安装、升级和运行入口全部使用同一校验。
- [x] Bot 路径函数只接受已校验 ID；非法本地目录和符号链接目录不得扫描或执行。
- [x] 安装、升级和每次执行前拒绝符号链接制品与下载临时文件。
- [x] 通过 Rust 格式、检查、Clippy 和 Bot 相关测试。

## 扫码配置（已完成）

- [x] 飞书 bot 在 schema 中声明扫码能力，App ID、App Secret 仅保留可选的手工配置入口。
- [x] 天工服务地址与认证 Token 从 bot schema 移除，启动时由主程序自动注入。
- [x] 在飞书 bot 中实现应用注册的扫码会话创建与状态轮询。
- [x] 扫码成功后由飞书 bot 自行保存并读取 App ID、App Secret，主程序不接收凭证。
- [x] 主程序通过通用子进程协议调用 bot 扫码能力，不包含飞书专属协议。
- [x] 增加桌面端通用扫码调用接口。
- [x] 配置表单展示二维码和授权状态，不回填或保存扫码所得凭证。
- [x] 支持等待、放慢轮询、拒绝、过期、网络失败与重新扫码状态。
- [x] 通过 Rust 检查、前端构建及真实飞书接口验证。

### 扫码配置完成标准

- 配置飞书 bot 时可直接生成二维码，不必先手工填写 App ID 或 App Secret。
- 授权成功后飞书 bot 自行保存凭证，主程序和前端均无法取得扫码所得凭证。
- 保存 bot 条目后可直接启动；飞书 bot 启动时从自己的配置中读取扫码凭证。
- 天工服务地址和认证 Token 不在动态配置表单中出现，且 bot 启动后仍可连接当前天工服务。
- 二维码过期或授权失败后可重新生成，不会误保存未完成的配置。

## 运行控制（已完成）

- [x] 删除 bot 列表和新增表单中的独立启用 Switch。
- [x] 启动 bot 时同时记录为随天工自动运行，停止时同时取消自动运行。
- [x] 启动按钮使用无外围留白的绿色三角，停止按钮使用无外围留白的红色实心方块。

## 配置交互（已完成）

- [x] 开始扫码后隐藏名称、手工配置字段和保存按钮，只保留扫码状态与取消入口。
- [x] 扫码成功且 bot 自行处理完配置后，自动注册或更新 bot 并关闭配置窗口。
- [x] 扫码配置保存后自动启动 bot，已运行时自动重启，确认运行后关闭窗口；保存或启动失败时保留窗口并支持对应重试。
- [x] 手工填写配置后点击保存，保存成功自动关闭配置窗口。

## Bot 展示与配置删除（已完成）

- [x] Bot 列表和操作提示使用制品清单中的展示名称，运行 ID 只作为内部标识和辅助信息。
- [x] 删除操作只移除 `bots.json` 中的配置并停止对应进程，保留已安装的 Bot 程序和运行目录。
- [x] 旧安装记录没有展示名称时仍可正常加载，并回退显示运行 ID。

## 日志查看（已完成）

- [x] 在已配置 bot 的操作区增加日志按钮。
- [x] 日志窗口展示当前 bot 的最近日志，支持刷新，并处理空日志和读取失败状态。
- [x] 日志读取限制返回大小，避免将完整轮转日志一次性载入界面。

## 运行状态一致性修复（已完成）

- [x] 启动成功后保存自动运行状态，保存失败时停止刚启动的 bot，并明确报告回滚结果。
- [x] 停止前取消自动运行，停止失败时恢复原配置；bot 已停止时重复停止仍视为成功。
- [x] 升级前记录配置和运行状态；升级期间停止 Bot，结束后恢复原配置和运行状态，失败时同时恢复旧制品。
- [x] 新制品下载到同目录唯一临时文件，完成 SHA-256 校验和 `--describe` 验证后再替换正式制品。
- [x] 应用启动时先确保 Server 启动并通过健康检查，再启动配置为自动运行的 bot；Server 不可用时不启动 bot。
- [x] 补充幂等停止、升级恢复和配置状态回滚验证。

### 运行状态一致性完成标准

- 启动成功后配置一定记录为自动运行，写入失败时不会留下仍在运行的 bot。
- 停止后配置一定取消自动运行，已经退出的 bot 也能正常修正配置。
- 升级期间 bot 必须停止；升级结束后恢复原有运行状态和自动运行设置，升级失败不会破坏旧制品。
- 应用重启时只有在 Server 健康后才自动启动 bot，实际运行状态符合配置。
- Rust 格式、检查、严格检查、相关测试及前端构建全部通过。

## 开发模式退出修复（已完成）

- [x] 开发构建监听一次 `Ctrl+C`，并通过主程序正常退出流程关闭应用。
- [x] 主程序退出前等待 bot 停止，Server 和前端开发服务随运行命令一并结束。
- [x] 使用真实 `cargo tauri dev` 启动并验证一次 `Ctrl+C` 后无残留进程和监听端口。

## 已完成

### 日志文件持久化

- [x] stdout、stderr 合并写入 `~/.tiangong/bots/<bot-id>/bot.log`，每行标注来源（stdout/stderr）与时间戳。
- [x] 崩溃重启后继续追加（不覆盖已有日志），便于追溯完整运行历史。
- [x] 内存只保留最近 8KB 错误摘要（stderr 优先）供健康状态展示。
- [x] 写日志文件失败不导致 bot 退出，但在主程序日志（`tracing::warn`）中报告。

### 日志轮转

- [x] 单个 bot.log 达到大小上限（默认 10MB）时轮转，重命名为 `bot.log.1`。
- [x] 保留最近 N 个轮转文件（默认 3 个），超出数量的最旧文件删除。
- [x] 轮转发生在写入侧，对 bot 子进程透明（bot 只管写 stdout/stderr）。

## 完成标准

- `~/.tiangong/bots/<id>/bot.log` 包含 stdout+stderr 合并日志，每行可辨来源与时间。
- 长时间运行的 bot 日志文件不会无限增长（轮转生效）。
- 崩溃重启后日志追加而非覆盖。
- `health()` 展示的错误摘要来自内存 8KB 摘要，不读取完整日志文件。
- 写日志失败时 bot 继续运行，主程序日志有告警。
- `cargo fmt --all`、`cargo clippy -p tiangong-bots --tests -- -D warnings`、相关测试通过。

## 非目标

- 日志按级别过滤（bot 日志是自由文本，不做结构化解析）。
- 远程日志收集。

# 用户输入框内联提及优化（已完成）

## 已完成

- [x] 输入框和已发送消息使用一致的内联标签展示 Agent、Skill、MCP 与全体提及。
- [x] 提及标签保持原始发送文本不变，并支持光标跨越、整块删除、中文输入、粘贴和多行输入。
- [x] 修复补全后光标被旧位置拉回，以及左右跨越分隔、整块删除与鼠标精确点击的边界问题。
- [x] 输入内容在 60px 到 200px 之间自适应，长内容可滚动且不横向溢出。
- [x] 编辑历史用户消息时复用相同交互，不影响附件和重新发送流程。
- [x] 通过现有相关检查、前端正式构建及桌面和窄屏真实交互验证。
- [x] 使用现有 Vitest 和 jsdom 覆盖中文组合输入、标签边界删除、连续标签移动、跨标签删除和多行粘贴，不新增浏览器测试依赖。
- [x] 覆盖候选选中后的光标位置，以及历史消息内容在提及编辑器中的修改。
- [x] 完成一次真实页面交互复验，确认中文输入和鼠标精确点击边界无回退。

## 完成标准

- 从补全菜单插入提及后，光标停在标签后方并可继续输入，不发生跳动或内容丢失。
- Backspace、Delete 和左右方向键在标签边界行为稳定，标签一次删除且相邻文字保留。
- 输入框、历史消息编辑框和已发送消息中的提及标签外观一致。
- 普通文本、邮箱地址、换行、附件粘贴和发送内容保持原有行为。
- 测试可直接复用现有前端环境运行，不需要下载或维护额外浏览器。

# 定时任务消息呈现优化（已完成）

## 已完成

- [x] 严格识别调度器生成的定时任务消息，普通用户消息保持原样。
- [x] 在消息列表中清晰展示任务名称、描述和执行内容，并保留搜索与复制能力。
- [x] 完全隐藏定时任务消息的编辑入口，并在编辑流程中增加保护。
- [x] 调整消息高度与导航预览，完成桌面和窄屏真实交互验证。
- [x] 通过现有前端检查与正式构建。
- [x] 使用现有轻量测试覆盖消息解析、搜索位置、专用展示和禁止编辑，不增加新的测试依赖。

## 完成标准

- 定时任务消息无需阅读内部标记即可识别任务来源和具体内容。
- 定时任务消息可以复制、搜索和定位，但无法进入编辑状态。
- 普通用户消息的展示、编辑、附件和搜索行为不受影响。

# 定时任务名称与描述单行化（已完成）

## 已完成

- [x] 创建和更新定时任务时，将名称、描述中的 Unix、Windows 及单独回车换行统一替换为单个空格。
- [x] 合并连续换行并清理首尾空白，处理后为空的名称或描述不得保存。
- [x] 人工创建、Agent 工具和服务接口使用相同规则，任务执行内容保持原样。
- [x] 通过定时任务相关格式、测试和严格检查，不改动消息格式及前端解析。

## 完成标准

- 名称和描述保存后始终为非空单行文本，换行前后的内容不会粘连。
- 创建与更新返回保存后的实际内容，所有入口表现一致。
- 多行任务执行内容不受影响，现有定时任务消息呈现保持不变。

# #255 文件锁迁入 fs 插件 TODO

## 已完成

- [x] 删除 agent-team 插件的 `state/file_lock.rs`、`FileLockManager` 字段、`lock_file`/`unlock_file`/`release_agent_locks`/`resolve_lock_path` 方法、`TOOL_LOCK_FILE`/`TOOL_UNLOCK_FILE` 常量与 ToolSpec、提示词中的加锁描述。
- [x] fs 插件接管文件锁（第一版，实例字段）。
- [x] 按评审反馈重构为进程级共享锁表。

### 进程级共享锁表（重构）

- [x] `file_lock.rs` 改为进程内全局锁表（`OnceLock<Mutex<HashMap<PathBuf, LockRecord>>>`），以规范化绝对路径为 key。
- [x] 锁记录只保存 `acquired_at` 与 `operation_id`（scru128），不再保存 Agent 身份。
- [x] 加锁语义改为「文件只要已有锁即拒绝后续写入」，不区分是否同一 Agent。
- [x] `operation_id` 用于解锁校验：旧操作超时后新操作取得锁，旧操作结束时不得误删新操作的锁。
- [x] 软链接路径归一：对不存在的新文件，向上找最近的已存在父目录 `canonicalize` 后再拼剩余路径，避免软链接产生两个锁标识。
- [x] `apply_patch` 先收集、去重全部目标文件，再一次性检查锁定；任一文件被占用则全部不加锁（全有或全无）。
- [x] 正常完成或失败后释放本次操作取得的锁；过期记录在下次加锁时静默清理。
- [x] `FileLockChanged` 事件只发送路径及「锁定/解锁」动作，`holder_agent_id`/`holder_agent_label` 保持为空。
- [x] 移除 `FsPlugin` 实例内的 `file_locks` 字段，改为调用全局锁表。
- [x] 测试覆盖：两 fs 插件实例写同一文件互斥、同一 Agent 并发写入互斥、软链接路径归一、过期后重新加锁、旧操作结束后不误删新操作的锁。
- [x] `cargo fmt --all`、`cargo check --workspace`、`cargo clippy`（`-D warnings`）、相关测试全部通过。

## 完成标准

- `lock_file`/`unlock_file` 工具及提示词从 agent-team 插件移除；`FileLock`/`FileLockManager` 迁至 fs 插件。
- fs 插件三个写工具自动加锁/解锁，对模型透明（无新工具暴露）。
- 同进程内任意两个 fs 插件实例（主 Agent 与子 Agent）写同一文件互斥。
- 过期锁（默认 30s 兜底）在下次加锁时静默清理，新操作可重新取得锁。
- 旧操作结束后不会误删新操作取得的锁（靠 `operation_id` 校验）。
- 软链接路径不会产生两个不同的锁标识。
- `cargo fmt -- --check`、`cargo check --workspace`、`cargo clippy --workspace --all-targets --tests --benches -- -D warnings`、相关测试全部通过。

## 非目标

- 多进程同时打开同一工作区的互斥（需系统级文件锁，改动范围明显扩大，留作后续独立 issue）。

# #241 上下文压缩闭环调整 TODO

## 已完成

- [x] 压缩阈值改为“5%输出预算 + 1%安全余量”的派生值，单轮复用同一计算器。
- [x] 压缩提示词声明实际输出预算，并优先输出当前任务状态。
- [x] 将 Provider 停止原因传回 Core，拒绝空摘要和被截断摘要。
- [x] 压缩任务使用 Session 快照，成功持久化后再提交真实会话。
- [x] 自动压缩和手动压缩支持 Cancel/Shutdown。
- [x] 当前任务续接以 User 消息写入 Session，保持模型可见，并在前端展示和搜索中隐藏。
- [x] 压缩成功、失败和取消均发布完整界面反馈。
- [x] ContextCompressor 只负责生成摘要，react/compression 统一处理任务生命周期、结果提交、用量和通知。
- [x] 压缩用量进入本轮最终用量，压缩后的当前 token 使用实际输出校准。
- [x] 补齐预算、截断、持久化续接、连续压缩、取消和失败原子性测试。
- [x] 通过 Rust 检查、相关测试和前端构建。
- [x] 强制压缩只折叠较早历史，保留最近一组完整交互，避免超限请求原样进入压缩请求。

## 完成标准

- 200k、1M 和更大上下文使用同一比例算法，不存在固定模型长度分支。
- 压缩失败或取消不会推进摘要边界。
- 压缩后的模型上下文以最新续接消息开头，连续压缩不会暴露旧续接消息。
- 前端保留完整原始对话，但不展示、不搜索或编辑续接消息。
- 所有检查通过后再更新本节任务状态。

# #238 Core 资源模型重构 TODO

## 已完成

### 核心资源模型
- [x] 共享 tokio runtime(`shared_runtime.rs`)替代 per-Core 线程+runtime
- [x] 每 turn 现建 engine/client(配置正交,不跨 turn 复用)
- [x] 转发屏障改 async 通知(`tokio::sync::Notify`),移除 `block_in_place`
- [x] 移除持久化回执(`PreparedMessageReceipt` / `SessionMetadataReceipt`)
- [x] 非 turn 命令归 app(`UpdateCwd` / `UpdateSessionMetadata` / `ReloadConfig` 移除)

### TurnContext 提取
- [x] 合并 `ReactEngine` + `RuntimeEngine` 为 `TurnContext`
- [x] 删除 5 个死字段(`models_config` / `core_config` / `lite_client` / `tool_spec_providers` / `runtime_env`)
- [x] `TurnContext` 持有 `session` 字段
- [x] `TurnContextBuilder` 替代 `build_turn_context` 函数

### 权限简化
- [x] 删除 `PermissionGate` / `PermissionPolicy` / `TrustModeHandle` / `PermissionLevel` / `PermissionDecision`
- [x] 权限改为二元判断:FullTrust 放行一切,否则统一走审批(审批在 turn 层完成)
- [x] 删除 `classify_tool` / `tool_permission_overrides` trait 方法 / `evaluate_tool_permission`
- [x] 删除 `PathRule` / `NetworkRule` / `check_path` / `check_network`(死配置)
- [x] 插件 `set_trust_mode` 参数 `TrustModeHandle` → `TrustMode`(Copy)
- [x] `Command::SetTrustMode` 即时生效(通过 cmd_rx select! 更新 ctx.trust_mode)

### Observer 注入
- [x] `Observer` 结构体(持有 `storage_root`)注入 `TurnContext`
- [x] 审计函数从全局函数改为 `Observer` 方法
- [x] 删除 `audit.rs` + `observe/audit.rs`,合并为 `observe/mod.rs`

### 全局变量清理
- [x] 删除全局 `STORAGE_ROOT`(`storage.rs`)
- [x] 删除审批持久化(`approval_store.rs`)— 审批改为 turn 内瞬态
- [x] `approval_store` 函数改为参数传入 → 随文件删除
- [x] `Session::new_isolated` 接收 `storage_root` 参数
- [x] `Session::try_persist_to_disk` 不再回退全局,要求预先 bind

### Turn task 模型(进行中)
- [x] `shared_runtime` 新增 turn task 管理(`spawn_turn` / `send_command` / `is_running` / GC)
- [x] `TURN_TASKS` 改为 `HashMap<session_id, (cmd_tx, JoinHandle)>`
- [x] `TiangongCore` 结构体重写:删除 `worker_task` / `command_delivery_lock`,新增 `plugins` / `external_tx` / `storage_root`
- [x] `deliver(Message)` → `spawn_turn`(构建 TurnContext + 注入用户消息 + 落盘)
- [x] `deliver(Cancel/Approval/SetTrustMode)` → `send_command`
- [x] `is_stopped` / `is_busy` → 查 `shared_runtime::is_running`
- [x] Desktop 将 Core 实例存在与 turn 运行状态分离，空闲 Core 仍可接收下一条用户消息
- [x] `into_session` → 从磁盘 load
- [x] `builder.rs` 重写:删除 `.session()`,新增 `.session_id()` / `.trust_mode()` / `.storage_root()`
- [x] `run_turn` 函数实现(替代 `worker_loop_async` 的 Message 分支)
- [x] `TurnContextBuilder` 创建(替代 `build_turn_context` 函数)

## 待完成

### Turn task 模型收尾
- [x] 删除 `worker_loop_async` 及不再使用的旧 worker 辅助逻辑，保留的轮次转发逻辑统一由 `run_turn` 持有
- [x] 删除 `build_turn_context` 函数，`TurnContext` 直接使用 `TypedBuilder`
- [x] 删除 `build_context_from_config` 函数
- [x] `deliver(Message)` 在 `spawn_turn` 前完成用户消息落盘并立即发布 `UserMessage`，再构建 `TurnContext`
- [x] Session 用户消息写入覆盖内容校验、同 ID 处理、失败回滚与完整落盘，删除 `accept_prepared_user_message_with_options`；遗留工具调用统一由上一轮取消收尾闭合
- [x] `run_turn` 从 `ctx.plugins` 调用插件生命周期钩子
- [x] `run_turn` 的插件钩子(`on_turn_started` / `on_turn_finished` / `on_cancel`)使用 `&mut session`
- [x] 删除 `run_turn` 结束时的插件工作区二次同步，插件工作区只在 turn 启动前设置
- [x] 删除 `run_turn` 的无效 `Session` 占位替换，以及 `react/message` 中仅转发底层方法的包装
- [x] `Session::close_unfinished_tool_calls_with_reason` 在补齐悬空工具消息后立即落盘；补齐落盘失败时删除悬空调用并重新落盘，成功后再由调用方发布实际存在的失败 `ToolResult`
- [x] 取消后不在 `run_turn` 收尾阶段刷新延迟工具注入，暂存数据保留到下一 turn 安全点
- [x] 整理 `run_turn` 收尾流程，在轮次锚点、插件生命周期、执行、消息修复、持久化与终态发布节点补充说明
- [x] `run_turn` 启动旁路计时器，每秒通过 `stream_tx` 发布当前运行秒数，并在提交最终耗时前停止
- [ ] App 对已有对话或执行记录的会话禁止修改工作区
- [x] 收敛单一终态合同：`execute_turn` 返回明确执行结果，最终持久化后只由 `run_turn` 发送一次 `Done` / `Error`，宿主按终态重载 Session，不再等待额外消息事件
- [x] `spawn_turn` 接收已构建的 `TurnContext` 与 Future 构建闭包，内部创建本轮 `cmd_tx / cmd_rx`
- [x] `spawn_turn` 在创建 Future 前遍历 `ctx.plugins` 调用 `set_feedback_tx`，并将 `cmd_tx` 存入 `TURN_TASKS`
- [x] 插件 `on_session_ready` 与提示段落注入调整到 feedback 绑定之后、turn task 启动之前
- [x] `deliver(Message)` 删除本轮命令通道创建与 `cmd_tx` 传参
- [x] 删除 `std::mem::forget(turn_cmd_tx)` 占位逻辑
- [x] 删除 `TiangongCore.session_ready_fired`，每轮 Session 加载完成并绑定 feedback 后、收集提示段落与执行 Agent Loop 前调用 `on_session_ready`
- [ ] `on_session_ready` 只负责基于本轮 Session 刷新插件状态；插件自行保证一次性后台初始化不重复，重点处理 Agent Team `Coordinator::initialize` 的幂等性

### execute_turn 内部简化
- [x] 将 `TurnContext::execute_turn` 从 Context 成员方法改为 `react/execute.rs` 的独立基础函数，并删除失去独立职责的 `engine` 模块
- [x] 将 `execute_turn` 迁入独立模块，按命令处理、模型请求、响应处理、工具执行与总结步骤拆分 Agent Loop
- [x] 将 `start_tool_call` 从 `TurnContext` 抽离到独立的 ReAct 工具执行模块
- [x] `execute_turn` 从 `ctx.session` 取得本轮 Session，不接收额外 Session 参数
- [x] 明确职责边界：`deliver(Message)` 完整构建本轮 Session，`run_turn` 只负责执行、收尾与持久化
- [x] 删除 `AcceptedUserMessage` 及 `run_turn` 的对应参数，统一从 `ctx.session` 使用本轮用户消息、消息 ID 与轮次起点
- [x] 删除 `execute_turn` 的 `initial_user_message` 参数，当前用户输入统一从 `ctx.session` 读取
- [x] `run_summary_phase` / `force_final_response` 同理改用 `self.session`
- [x] 合并 `execute_agent_loop` 与 `execute_turn`，由 `execute_turn` 直接编排并返回本轮结果
- [x] 将 `execute_react_phase` 平铺进 `execute_turn`，固定为外层 `react_loop`、内层 `execute_loop`，在各阶段等待点直接监听运行时命令；不使用包裹整轮的 `execute_future`，不再抽离阶段编排方法或增加阶段结果转换
- [x] 将 `execute_turn` 的内层 `execute_loop` 原样抽离为独立方法，保留外层 `react_loop` 的总结重入编排与既有取消、命令、用量语义
- [x] 修正运行时插件注入合同：接收后立即向 App 发布待处理快照，在安全边界完整写入 Session，取消前已接收的数据不得丢失，且晚于当前请求到达的结果必须由 Agent 在后续请求中消费
- [x] 将 `execute_turn` 收敛为唯一事件循环和唯一 `cmd_rx` 接收者：在同一个 `tokio::select!` 中处理命令、模型流与工具执行，保留工具阶段轮次上限、总结重入上限、取消传播、实时插件反馈和用量累计语义
- [x] 同一 LLM 回复中的工具调用使用 Tokio 并行执行；每项完成后立即向 App 反馈、写入并持久化 Session，全部工具结束后再继续 Agent Loop，取消时统一终止并闭合未完成调用
- [x] 执行链统一只传 `TurnContext`，通过 `ctx.session` 访问会话，删除占位 Session 与 `ctx + session` 双参数
- [x] 删除 `TurnUsageSink` 与 turn 绑定旁路；插件用量统一通过 `PluginFeedbackTx` 命令上报，并直接累计到 `execute_turn` 本轮用量
- [x] 删除基于关键词的后台工具意图过滤和 `user_input` 局部状态，所有工具统一交由主模型结合 Session 上下文选择
- [x] 删除未参与控制流的 `TurnPhase` 枚举和无效阶段赋值，阶段变化只通过 `StreamEvent::PhaseChanged` 发布
- [x] 删除 `PendingCommandEffect`、运行中消息追加链路及通用命令处理封装；主 `cmd_rx` 直接在 `execute_turn` 外层 `react_loop` 中展开，收到 `Cancel` / `Shutdown` 后立即关闭接收端，并通过逐层新建的 `oneshot` 通知内层 `execute_loop` 由内向外收尾退出
- [x] 补齐 `execute_turn` 的请求失败、运行时命令、审批、工具/总结取消、总结重入与强制收尾测试，并用覆盖率报告复核关键控制分支
- [x] 补充 `execute_turn` 关键执行节点注释，说明双层循环、命令处理、取消传播、工具执行、总结重入与结果出口，不改变执行逻辑
- [x] 收紧忙碌期控制边界：手动压缩与清空上下文仅在 Core 空闲时执行，`execute_turn` 只接收取消、信任模式切换、审批及内部反馈；信任模式立即更新运行态并随轮次统一落盘；手动压缩复用 `spawn_turn`，并与自动压缩统一使用滚动摘要流程
- [x] 为 `TiangongCore` 增加无参数的 `build_turn_context`，内部加载 Session；普通投递统一通过 `ctx.session` 写入用户消息
- [x] `reset_context` 和手动压缩结束时不重建系统提示，下一轮对话启动时再统一重建
- [x] 删除手动压缩开始前的重复落盘，并让手动与自动压缩统一使用 `maybe_update_context_summary`
- [x] 简化手动压缩：使用 `Session.context()` 取得安全消息并按轮弹出，`maybe_update_context_summary` 只接收待压缩消息，由压缩器内部推导持久化边界
- [x] 拆分压缩阈值判断与压缩执行收尾，手动压缩直接执行且不再伪造用量

### 调用方适配
- [ ] `app.rs ensure_core`:先 persist session 文件再创建 Core(不再传 session 给 builder)
- [ ] `app.rs create_core`:适配新 builder API(`.session_id()` / `.trust_mode()` / `.storage_root()`)
- [ ] `app.rs retire_core`:`shutdown_join` 简化(不再等 worker task)
- [ ] `child_runtime.rs`:适配新 builder API + `deliver_and_wait` 适配 turn task 模型
- [ ] `CLI repl.rs`:`into_session` 从磁盘 load(不再从 worker 取回)
- [ ] `embedded_server.rs`:适配新 builder API

### 后续独立 issue
- [x] #245 app-state 仅保留本次运行状态(session 真相源归磁盘,移除完整 Session 列表 / RuntimeEngine / save_core_session / app.json 持久化)
- [x] `TiangongState` 启动时加载完整配置并据此创建 `CoreManager`，Desktop / CLI / Server 共用该实例
- [x] 新对话只预留最终 Session ID 和输入缓存，首次向 Core 投递消息时才创建并保存 Session
- [x] 将默认信任模式、默认工作目录和自定义 Prompt 纳入配置，并兼容旧 `app.json`
- [x] 修复 Desktop 运行中卡顿与流式输出、从模型 `context_window` 派生上下文与压缩阈值、终端 Tab 恢复目录错误
- [x] 修复 ReAct 流式 Markdown 继承纯文本换行样式后产生额外空行的问题，流式与完成态排版保持一致
- [x] Agent 运行时间只使用 `stream_tx` 返回的 `TurnElapsed`，移除前端本地计时
- [ ] app-state 的 `RuntimeEngine` 替换为轻量配置结构体
- [ ] `Command::Message` / `AgentInputKind::Message` 双层映射合并


# 跨平台嵌入浏览器凭据能力验证

## 需求与安全边界（已完成）

- [x] 明确 Windows、macOS 用户名密码填充与 WebAuthn 的平台差异。
- [x] 明确不读取 Edge、Safari 或 iCloud 密码库明文，不向网页脚本、日志、模型或插件暴露密码。
- [x] 明确 `edge://settings` 等浏览器内部页面不作为天工设置入口。
- [x] 形成 `docs/browser/06-credential-capability-validation.md` 验证方案。

## Windows 密码保存验证

- [ ] 创建独立任务分支，在 WebView2 初始化后开启密码自动保存。
- [ ] 保持普通表单自动填充开启，并验证它与密码保存开关彼此独立。
- [ ] 移除 Windows 上固定的 macOS Safari 风格用户代理，使用 WebView2 默认值。
- [ ] 在受控 HTTPS 表单验证保存、更新、建议和填充。
- [ ] 验证同一会话多标签、应用重启和不同会话的数据边界。
- [ ] 回归 GitHub WebAuthn、Windows Hello、跨设备通行密钥和可用的 FIDO2 设备。

## macOS 密码填充验证

- [ ] 使用标准用户名、当前密码、新密码表单验证系统 Password AutoFill。
- [ ] 验证 Safari/iCloud 中已有凭据的建议与 Touch ID 确认行为。
- [ ] 验证保存新密码和更新密码提示是否出现，并记录 macOS 版本差异。
- [ ] 验证 GitHub、Google 等实际登录页，不将 Associated Domains 作为任意网站方案。
- [ ] 系统填充不可用时验证“在默认浏览器中打开”兜底。

## macOS 通行密钥验证

- [ ] 普通开发构建记录 GitHub 和 `webauthn.io` 的部分支持行为。
- [ ] 确认 Apple Developer Program、App ID 与正式签名条件。
- [ ] 申请 Web Browser Public Key Credential Requests 受限能力。
- [ ] 使用包含受限能力的正式签名配置构建并核对最终应用权限。
- [ ] 验证 Touch ID、iCloud 通行密钥、跨设备登录及可用的硬件安全密钥。

## 设置与清理

- [ ] 提供密码保存和普通表单填充开关，仅在平台支持时展示。
- [ ] 提供当前浏览身份说明和清除浏览数据入口。
- [ ] 提供在默认浏览器中打开当前页面的入口。
- [ ] 不提供密码明文查看、导出或系统浏览器密码库读取能力。

## 完成标准

- Windows 可以在天工自己的持久化浏览身份中保存并再次填充用户名密码，WebAuthn 不回退。
- macOS 的密码填充、密码保存和通行密钥能力都有真机可重复结果，并清楚标注系统限制。
- 两端失败时均有安全、明确的默认浏览器兜底，不使用脚本处理密码。
- Rust 检查与前端正式构建通过；Windows、macOS 真机结果分别确认。

# macOS WebView 空 URL 崩溃修复

## 依赖热修

- [x] 将 wry 锁定为自有 fork 的已验证提交，且版本保持为 `0.55.1`。
- [x] 更新锁文件并确认 Desktop 只解析到一份修复版 wry。

## 验证

- [x] 通过 Rust 格式、浏览器插件测试和 Desktop 编译检查。
- [x] 使用空 URL 最小场景确认应用不再因 WebView 取址失败退出。

## 完成标准

- WKWebView 导航期间暂时没有 URL 时，天工保持运行，不写入空标签地址。
- 无效 URL 仍按现有等待与错误路径返回，不因延长等待掩盖底层异常。
- Windows、Linux 和移动端继续使用相同的 wry 版本与原有平台实现。

# 浏览器导航失败白屏修复

## 方案设计

- [x] 核对导航失败、超时和底层取消的现有调用链。
- [x] 明确 wry 只保留空 URL 防崩溃补丁，页面加载异常统一由天工按截止时间处理。
- [x] 形成 `docs/browser/07-navigation-failure-recovery.md` 详细修复方案。

## wry 变更边界（已完成）

- [x] 继续锁定现有空 URL 防崩溃提交，不接入额外的 wry 失败回调和错误页提交。
- [x] 保持 Windows、Linux、移动端及 wry 公共接口不变。

## 天工状态闭环

- [x] 为每个会话和标签维护带 navigation ID 的加载结果状态。
- [x] 页面完成时按 navigation ID 和实际文档编号结束当前等待并写入成功状态。
- [x] 页面状态与页面快照使用独立 JS 文件，并在 Rust 中编译期静态引入。
- [x] 使用固定 30 秒导航截止时间，Agent 命令通道额外保留 5 秒。
- [x] 保留请求地址，过滤错误页内部地址和正文。
- [x] 标签导航记录和全局浏览历史使用独立规则，标签历史保留重复访问，全局历史只记录成功页面。
- [x] 普通导航、后退、前进、刷新、失败重试和重定向按明确意图更新当前标签记录。
- [x] Agent 按主域名复用带来源标记的工作标签，不覆盖用户标签，重定向和重试保持原标签。
- [x] 同一标签快速连续导航时，旧加载事件和截止任务不得更新当前导航。
- [x] 合法重定向不按旧地址拒绝，并与最初主动导航共享同一截止时间。
- [x] 前端支持失败状态、重新加载及正常的后退、前进和切换。
- [x] Agent 在失败时返回明确错误，不把错误页作为成功内容注入。
- [ ] 保持现有 wry 精确提交并完成桌面端、前端和 macOS 真机验证。

## 完成标准

- 导航失败、超时和终态取消后均显示错误页，不再持续白屏。
- 正常连续导航不会被旧请求取消误报为失败。
- 地址栏、标签历史、全局历史和 Agent 结果保持一致。
- 错误页可以重试并恢复到正常页面。
- 进程不退出，Windows、Linux 和移动端现有行为不回退。


## 当前任务：Index 与 Scheduler 插件设置 UI 对齐

- [x] Index WASM 设置页恢复原版索引管理的信息层级、状态、刷新、重建和删除交互。
- [x] Scheduler WASM 提供新版动态设置页，恢复原版任务列表、刷新、新建编辑、启停、触发、删除、执行历史和双模式 Cron 编辑；宿主固定定时任务入口继续保留。
- [x] Scheduler sidecar 优先读取宿主 `storage_root` 下既有的 `scheduler` 数据目录，确保固定入口与插件入口共用原任务和执行记录。
- [x] 所有业务请求仅通过插件 `view-message` 与各自 sidecar 通信，不修改宿主前端、Tauri 或 Server。
- [x] 页面适配宿主明暗主题、完整高度和 iframe 内滚动，错误与操作结果在插件页内可见。
- [x] 重新构建 Index、Scheduler WASM 与完整插件制品并通过相关代码检查。

### 完成标准

- Index 与 Scheduler 动态插件页的功能、信息和主要交互与迁移前页面一致。
- 改动范围仅包含 `PLAN.md`、`TODO.md` 及两个插件目录。
- 两个 WASM 组件和插件制品可正常构建。
