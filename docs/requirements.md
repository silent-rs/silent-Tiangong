# 天工需求整理

## 文档目的
用于对齐天工全栈平台重构（RFC 0004）的开发边界，作为 `PLAN.md`、`TODO.md` 与实现代码的一致性基线。

## 已有基线能力

以下能力视为当前稳定基线，重构过程中必须保持不回退：

- `tiangong` 默认保持桌面 GUI 入口（Tauri + React + shadcn/ui），CLI 入口为 `tiangong cli`。
- CLI 具备任务闭环：输入 -> planning -> executing -> final response。
- MCP 已支持本地 `stdio` 与远程 HTTP（JSON-RPC over HTTP）并存。
- Agent 配置已支持查看、更新、校验与即时生效。
- Skills 已支持本地安装、启停、卸载与按任务意图匹配。
- 动态 Step 执行闭环已实现。

## 当前范围（RFC 0004：全栈个人智能终端平台）

### Must

#### Workspace 拆分
- 项目必须从单 crate 重构为 Cargo workspace 多 crate 结构。
- `tiangong-core` 必须作为独立 crate，不依赖任何 UI 框架。
- `tiangong-cli` 和 `tiangong-gui` 必须作为独立前端 crate，依赖 `tiangong-core`。
- 重构后现有 GUI 和 CLI 功能必须完整保留，不允许功能回退。
- 必须提供 `TiangongCore` 的正式对接文档，覆盖最小配置、初始化方式、事件流消费、会话恢复与热更新接入说明，供 CLI/GUI/Server 与第三方嵌入方统一参考。

#### 发布与分发
- 必须提供 GitHub Actions 发布流水线，支持手动触发和 `v*` 版本标签触发。
- 发布流水线必须构建 Tauri 桌面安装包，并将 macOS、Windows、Linux 产物上传到 GitHub Release。
- 发布流水线必须使用仓库内前端与 Tauri 配置完成构建，不依赖开发者本机环境。
- 手动触发发布时必须默认创建草稿 Release，便于发布前检查安装包。
- 应用必须通过 GitHub Release 的 updater JSON 检测新版本并执行应用内更新。
- 设置界面必须展示当前应用版本，并提供检查更新与安装更新入口。
- `tiangong update` 必须复用同一 GitHub Release 在线更新链路；桌面打包二进制应支持命令行触发检查、下载和安装更新。
- 更新包必须使用 Tauri updater 签名校验，私钥只能通过 GitHub Secrets 注入发布流水线。
- updater JSON 中的更新说明必须随版本内容生成，避免长期复用固定描述。

#### 模型配置
- 模型配置必须独立为 `models.json`，采用 Provider 与 Model 分离设计。
- Provider 层只定义连接信息（`base_url`、`api_key`、`timeout_ms`），可被多个模型共享。
- Provider 层必须支持声明请求协议类型，至少包含 `openai_chatcompletions`、`anthropic` 与 `deepseek` 三种协议；未显式配置时默认按 `openai_chatcompletions` 处理，并兼容历史 `openai` / `openai_compatible` 配置。
- Model 层引用 Provider，声明实际模型 ID 和能力列表（`capabilities`），携带专属参数（`options`）。
- 能力类型包括：`chat`（常规对话/推理）、`lite`（轻量文本）、`multimodal`（多模态理解）、`embedding`（通用向量嵌入）、`rerank`（通用结果重排）、`image_generation`（图片生成）、`video_generation`（视频生成）、`stt`（语音识别）、`tts`（语音合成）。
- 一个模型可声明多种能力（如 gpt-4o 同时支持 chat 和 multimodal）。
- `routing` 表只管理对话和多媒体运行能力的默认模型，不包含 `embedding` 和 `rerank`。
- `chat` 为基础必选能力，routing 中未配置 `chat` 时程序必须在所有模式（GUI/CLI/Server）下持续提示用户完成模型设置，在设置完成前不执行对话任务。
- Memory 模型配置不属于主 `models.json` routing；前端必须放在 LLM 配置下，通过已有 Models 选择 Memory LLM、Embedding、Rerank，并由后端解析后写入 `~/.tiangong/memory/config.json`。其中 `embedding` 和 `rerank` 只作为 Models 能力标签供 Memory 选择，不进入 Routing 页。
- 其余能力（multimodal/image_generation/video_generation/stt/tts）未在 routing 中配置时视为关闭，对应功能不可用但不影响其他功能正常运行。
- 当配置 `multimodal` routing 时，GUI 输入区必须开放图片/文件附件入口；附件进入会话后不得导致主对话请求自动切换到多模态模型。主模型仍负责对话、推理和工具决策，并在确实需要查看附件内容时主动调用附件解析工具，由该工具调用多模态模型完成解析。
- `api_key` 必须支持环境变量引用（`${ENV_VAR}` 语法），避免明文存储。
- 核心对话链路必须按 Provider 协议动态构建请求，不允许将所有聊天模型强制视为 OpenAI 兼容接口。
- 当 `chat` 或 `lite` routing 指向 `anthropic` Provider 时，必须支持 Anthropic Messages 请求格式的同步、流式、工具调用与轻量模型调用，并保持 CLI/GUI/Server 行为一致。
- 当 `chat` 或 `lite` routing 指向 `deepseek` Provider 时，必须支持 DeepSeek Chat Completions 请求格式的同步、流式、工具调用与轻量模型调用，包括思考模式（thinking）控制、reasoning_effort 映射（Low/Medium/High → high，Max → max）以及上下文缓存 token 报告，并保持 CLI/GUI/Server 行为一致。
- OpenAI 协议层当前主线统一使用 Chat Completions API（`/chat/completions`，协议值 `openai_chatcompletions`，默认协议）；历史 `openai` / `openai_compatible` 配置必须恢复到 Chat Completions 行为，避免误触发暂未稳定的 `/responses` 路径。Responses API 适配暂放独立分支继续验证，未合入主线前不得作为主链路可选协议暴露。
- Rust 侧 LLM 协议层必须沉淀为独立库（当前为 `tiangong-llm`），由该库统一维护 Provider 抽象、领域模型与错误边界；Anthropic 适配层必须拆分为独立 crate `tiangong-anthropic`，DeepSeek 适配层必须拆分为独立 crate `tiangong-deepseek`，由该 crate 内部使用 `reqwest + serde` 原生实现 DeepSeek Chat Completions 与 SSE 解析，并通过 `tiangong-llm` 接入上层；OpenAI 兼容适配层内部可接入 `async-openai`，但第三方 SDK 类型不允许泄漏到上层业务。
- 必须维护 `tiangong-llm` 的架构约束文档，明确其职责仅限统一抽象、Provider 封装、请求/响应映射、错误边界与流事件边界；新增协议或修复兼容问题时不得继续将 transport、业务规则和上层策略堆叠进该 crate，后续若需拆分 transport 或共享执行器，必须以该文档为准。

#### Server 模式
- 必须新增 `tiangong server` 命令，启动 HTTP REST + WebSocket 服务。
- 必须支持 `-d` / `--daemon` 参数后台运行（fork 进程后主进程退出，PID 写入 `~/.tiangong/server.pid`）。
- 必须支持 `tiangong server stop` 命令，读取 PID 文件停止后台 Server 进程。
- REST API 必须覆盖：对话、会话管理、Skill/MCP 管理。
- WebSocket 必须支持流式对话与事件推送。
- Server 模式必须支持 API Token 认证。
- 默认绑定 `127.0.0.1`，需用户显式指定才可绑定外部地址。
- Server 模式必须在运行时强制使用 `full_trust`，不允许进入运行中审批状态。
- Server 端不提供审批 API，不暴露远程审批事件，也不维护远程审批角色。
- Server 远程角色收敛为 `controller` 与 `observer`：控制者可发消息和管理会话，观察者只读。
- Desktop 设置页必须支持在应用运行时启动和停止后台 Server，并复用同一份 Server 监听地址、端口与认证 Token 配置。
- Desktop 模式下必须提供菜单栏 / 系统托盘入口控制后台 Server 的启动和停止；关闭主窗口时应用应继续驻留在菜单栏，保证已启动的后台 Server 持续存活，只有用户显式停止 Server 时才停止 Server。

#### 三方 IM 适配
- 与三方 IM 软件的交互通过 Server API + 外部适配程序（adapt）完成，不在天工内部实现 Connector 机制。
- 外部适配程序通过 Server REST API / WebSocket 收发消息，天工只负责提供标准 Server 接口。

#### 多媒体能力
- 必须支持图片生成能力，至少实现一个后端适配（OpenAI DALL-E / GPT-Image）；models.json 中未配置 `image_generation` routing 时该功能自动关闭。
- 必须支持视频生成能力，至少实现一个后端适配；未配置 routing 时自动关闭。
- 必须支持语音识别（STT）能力，至少实现 OpenAI Whisper 适配；未配置 routing 时自动关闭。
- 必须支持语音合成（TTS）能力，至少实现 OpenAI TTS 适配；未配置 routing 时自动关闭。
- 多媒体能力必须保留原生主链路，图片/视频/STT/TTS 不能统一退化为 MCP tool 文本输出。
- MCP 若参与多媒体实现，只能作为 `tiangong-media` 的后端适配来源之一，不能替代上层媒体结果语义。
- 插件/工具返回的结果供 Agent Loop 使用，不由前端直接展示；前端展示来自 Agent 的最终回复。
- 图片生成结果无论来自内置图片生成模型还是 MCP 工具，都必须优先下载或解码归档到本地 `~/.tiangong/media/images/`，会话中的结构化图片资源应引用本地文件路径，避免远程临时 URL 过期导致历史图片不可访问。
- 用户输入侧必须支持图片、文件等多模态附件；未配置多模态模型时入口隐藏或禁用，已配置时附件随用户消息进入会话层，但普通主模型请求只接收附件元信息，不直接携带附件原始内容。
- 未配置 `multimodal` routing 时，Desktop 必须关闭文件上传能力，前端不得保留或提交附件，后端也必须拒绝带附件的消息入口。
- Desktop 用户输入侧附件暂统一以 base64 data URL 形式提交；任一附件 base64 编码后超过 50MB 时，必须提示用户文件过大并停止发起请求。
- 用户输入侧包含附件时，本轮对话仍必须优先使用主 `chat` 模型处理；是否调用 `multimodal` routing 指定的模型解析附件、解析哪一个附件、如何解析，必须由主模型通过附件解析工具显式决定。
- 用户上传的图片和文件全部由入口层（GUI 的 app_state ingress、Server 的 remote/core）归档到本地 `~/.tiangong/media/` 后再投递给 Core，不是由会话 Core 插件归档。`MediaKind::Image` 走图片归档（`images/`），`MediaKind::File` 走文件归档（`files/`）；文件类型只影响保存方式和扩展名，不影响是否归档。
- 用户在消息中直接输入本地图片路径或普通网页 URL 时，不得触发自动多模态路由；只有用户上传为会话附件的内容，才可由主模型通过附件解析工具交给多模态模型处理。
- 系统必须能够区分“最终结果媒体”和“中间过程媒体”，不能将所有图片/视频工具结果一律视为最终回复。
- 媒体生成必须采用异步任务模型，支持状态查询。
- 生成的媒体文件必须可通过外部适配程序发送到 IM 通道。
- 外部适配程序接收到语音消息时，应负责调用 STT 转为文字后再提交给 Server API。
- Agent 响应时应支持可选的 TTS 输出，将文本回复转为语音发送。

#### 上下文管理
- 当 API 返回的精确 `prompt_tokens` 达到模型上下文限制的 95% 或用户配置的上下文长度阈值时，必须自动触发上下文压缩。
- Agent 主循环在收到 LLM 输出后，必须结合本次请求的输入与输出 token 总量判断是否触发上下文压缩；达到阈值时应先压缩早期上下文，再继续下一轮工具调用或回复生成。
- GUI 必须展示当前请求上下文 tokens、触发上下文压缩的进度条和当前会话累计总 tokens。
- 所有会话相关 LLM 请求产生的 token 消耗都必须进入会话统计，至少覆盖主对话请求、工具调用决策、附件解析、记忆召回和上下文压缩摘要请求。
- 压缩策略优先使用 LLM 摘要，保留最近 N 轮完整对话，对早期消息生成摘要；LLM 摘要失败时回退到滑动窗口截断。
- ReAct 循环内 loop_messages 累积过大时，必须对早期轮次的工具调用和结果进行摘要压缩，避免单轮执行 token 溢出。
- 对话链路必须按无状态 Chat API 的多轮对话模式组织上下文：每次请求由客户端按顺序拼接历史 user/assistant/tool 消息，并在末尾追加当前轮相关上下文。
- 高波动内容（如 recall_memory 结果、运行时日志、MCP 摘要）不得追加到稳定 system prompt；确需给模型参考时，应作为 user-side system-reminder 或工具结果进入消息链。
- 非用户直接提交的运行时上下文、工具反馈、LLM 输出记录、MCP 摘要和 recall_memory 注入必须在会话层使用 `tool` 类型消息承载；其中没有 `tool_call_id` 的内部 tool 上下文在发送给 Provider 时需转换为兼容文本上下文，避免构造非法 tool 消息。
- 上下文压缩产出的早期对话摘要必须注入 system prompt，不能作为普通 `tool` 消息参与消息链。
- 手动压缩、自动压缩和清空上下文后都必须按当前代码与配置重新生成 system prompt，避免旧会话继续携带历史版本规则。
- 用户自定义特色 Prompt 必须作为稳定 system prompt 组成部分注入；新对话和上下文压缩后的后续请求均必须继续包含该自定义 Prompt。
- GUI 必须在上下文压缩开始和完成时给用户可见反馈，避免长对话中压缩过程表现为无响应。

#### Agent 偏好与审核配置
- 应用配置必须支持设置新对话默认审核权限，至少包含完全信任与监督审核两种模式；该设置只作为新建会话的初始值，不得覆盖已有会话。
- 每个会话必须在会话记录中持久化自己的审核权限；用户在对话输入区调整审核状态时，只修改当前会话，切换会话、恢复会话和后台继续执行时均必须以该会话记录中的审核权限为准。
- 默认审核权限和用户自定义特色 Prompt 必须持久化，重启后继续生效。

#### Core 内置能力
- Core 必须提供 `current_time` 内置工具，返回当前本地时间、RFC3339 时间、Unix 时间戳和时区偏移，供模型处理“今天、现在、当前时间”等时效性请求。

#### Memory 系统
- Memory 功能必须可关闭，未启动或启动失败时主对话链路必须降级继续运行。
- Memory 必须保持独立 crate，不能依赖 `tiangong-core` 或 UI 层。
- Memory 必须复用 `tiangong-llm` 的文本生成、embedding 与 rerank 能力，不在内部重复实现 Provider 协议适配。
- Memory runtime 的模型配置必须独立于主对话 routing，由 `tiangong-memory` 自己定义配置类型并持久化到 `~/.tiangong/memory/config.json`；GUI 配置入口位于 LLM 配置下，模型选择复用主 LLM 的 Provider/Model 定义。
- Memory 的文本生成必须使用独立 Memory LLM 配置，不得使用主 `chat` 模型或轻量 `lite` 模型作为隐式回退；未配置专用 Memory LLM 时，Memory 相关 LLM 步骤必须降级为规则策略并记录可诊断日志。
- Memory 必须支持主模型按需调用的 Tool 化回忆，不能在每个 turn 前强制自动注入 recall 结果。
- Memory 收到 Tool 化回忆刺激后，必须先执行初始回忆，再基于初始结果判断是否需要 deep recall，不能把一次 `recall_memory` 调用等同为深度回忆。
- Memory 必须支持 workspace 隔离，长生命周期 GUI/Server 进程不得把不同 workspace 的记忆写入同一 scope。
- GUI、CLI、Server 必须通过 Memory leader election / IPC 获取当前 workspace 的 Memory handle；同一 workspace 只能有一个 leader，不同 workspace 的 leader 运行文件必须相互隔离。
- Memory 必须支持产物记忆，至少覆盖媒体 URL、文件路径、工具结果摘要和可继续使用的产物引用。
- GUI 必须提供 Memory 手动管理界面，允许用户查看当前 workspace 的记忆、手动新增或调整记忆摘要、归档记忆，并执行手动召回测试。
- 手动新增或调整的记忆必须通过 Memory Actor 写入，保证 SQLite、Tantivy 和可用向量索引一致；召回测试不得写入会话消息链。
- Memory 必须围绕 8 类核心记忆展开能力：事实性记忆、用户偏好记忆、用户习惯性记忆、技能型记忆、项目结构记忆、架构决策记忆、问题与故障记忆、领域知识记忆。
- Memory 必须具备类似图数据库的关系结构，允许不同记忆节点之间建立有向关系边，并在 deep recall 中沿关系边加载邻接记忆以支持深入回忆。
- GUI Memory 手动管理界面必须支持设置记忆类型、维护记忆之间的关联关系，并以高性能图谱渲染库展示当前 workspace 的圆形记忆节点和连接线；所有写入必须通过 Memory Actor，避免绕过 SQLite/索引一致性边界。
- Memory 必须具备独立集成测试，覆盖写入、召回、IPC、workspace 隔离、混合检索、产物记忆和配置热更新。
- Memory recall 输出必须有统一预算和去重策略，避免重复当前上下文、重复 URL、重复路径或重复工具结果摘要。
- Memory 运行时召回必须支持低成本粗回忆：用户消息进入、规划或执行过程需要历史线索时，先使用本地纯搜索引擎进行粗召回；只有粗召回不足以支撑当前操作时，才升级为混合检索、rerank 或 Memory LLM 参与的深度回忆。
- Agent 执行过程中如果当前上下文对即将执行的工具调用或失败恢复没有明显辅助效果，必须允许基于下一步操作意图重新回忆；重新回忆结果不得进入稳定 system prompt，应作为运行时工具上下文进入消息链，并受上下文预算控制。
- Memory 归档必须同步 SQLite 状态、Tantivy、内置向量索引和可选外部 Qdrant，避免已归档记忆继续被召回。
- Meta 反刍必须覆盖低活跃记忆、失效文件路径、过期产物 URL 和项目归档标记，并输出可观测计数。
- Workspace Index 必须支持最小文件树索引、Rust `mod/fn/struct/enum/trait` 符号索引、按 workspace 隔离查询和单文件增量更新。
- Tool 化回忆在返回长期记忆时应能补充相关 workspace 文件和符号线索；没有历史记忆但存在相关文件线索时，也应返回 workspace index 结果。

#### 多智能体协作
- 主 Agent 必须能够在会话中动态创建、解散 Sub Agent，并为每个 Sub Agent 维护独立角色、状态、工具范围和会话上下文。
- 持久 Sub Agent 必须在当前会话内跨用户消息保留，临时 Sub Agent 必须在单次任务完成后自动释放。
- Sub Agent 之间必须支持定向消息和广播消息，用户也必须能够通过 `@role`、多角色 `@role @role` 和 `@all` 将消息直接路由给存活 Agent。
- Sub Agent 必须能够通过通知事件直接向用户推送进度、阻塞、问题和错误信息，前端应能按 Agent 查看相关事件。
- 多 Agent 共享同一工作区时，Sub Agent 编辑文件前必须先获取文件锁；未持有锁时不得执行写入或替换，主 Agent 保留最高释放权限。
- 文件锁必须支持超时释放、Agent 销毁时释放和前端锁状态变更事件。
- 多 Agent 并发执行必须有数量和 token 预算上限，单个 Sub Agent 失败不得导致其他 Agent 或主会话崩溃。

#### 工作空间与文件操作边界
- Desktop 模式必须支持在界面中设置当前会话工作空间。
- CLI / Server 模式默认使用进程当前运行目录作为当前工作空间。
- 用户没有显式指定目录时，文件工具、命令工具和上下文加载必须默认以当前工作空间为基准。
- 文件读取、目录查看和代码搜索允许访问工作空间外路径，以便在必要时获取足够多的信息。
- 文件写入、修改、删除、补丁应用和会产生文件副作用的命令必须限制在当前工作空间、当前对话显式指定目录和 `~/.tiangong/skills` 范围内。
- `~/.tiangong/skills` 作为特殊可写范围保留，用于 Skill 出现问题时及时调整和修复。
- 除上述允许范围外，不允许工具在其他目录创建、修改或删除文件。

#### web_fetch 基础能力
- 必须提供 Core 内置 `web_fetch` 工具，作为未安装 `curl` / `wget` 时的受控网页获取与在线文件下载替代能力。
- `web_fetch` 必须支持 `text` 与 `download` 两种模式；`text` 模式返回网页或文本正文，`download` 模式将在线文件保存到允许写入目录。
- `web_fetch` 仅允许 HTTP / HTTPS URL，必须拒绝非 HTTP 协议。
- 默认策略下必须拒绝本机、私网、链路本地、保留地址和云元数据地址，重定向后的目标也必须重新检查。
- `text` 模式必须支持 HTML、纯文本、JSON、Markdown 和 XML 等常见文本内容，并对 HTML 做基础正文提取。
- `download` 模式必须复用现有写入边界，只允许写入当前工作空间、当前对话显式指定目录和 `~/.tiangong/skills` 等允许范围。
- `download` 模式必须限制最大下载大小，默认不覆盖已有文件，写入完成后返回文件路径、字节数、内容类型和摘要信息。
- `web_fetch` 结果必须作为结构化工具结果进入会话层，不得追加到稳定 system prompt。
- CLI、GUI、Server 必须通过同一 Core 工具链路获得一致行为，不允许前端重复实现抓取或下载逻辑。

#### 嵌入式浏览器智能定位
- 浏览器工具必须支持智能元素定位，除 CSS selector 外，还应支持按可见文本、ARIA label、role、表单 label、placeholder、name 和简单表格坐标定位元素。
- 当智能定位存在多个候选时，工具必须返回候选列表供 Agent 选择，不得盲目点击或填写不确定目标。
- `web_click` 和 `web_form_fill` 必须保持原有 `selector` 参数兼容性，同时允许该参数承载自然语言定位描述。
- 用户在页面上绘制批注后，`web_browse` 必须主动提取批注覆盖区域内的可见文本和主要元素摘要，并作为工具结果反馈给 Agent。

#### 嵌入式浏览器浏览历史
- 浏览器工具栏的后退/前进按钮必须支持多级历史导航，按钮旁须提供下拉箭头展示当前标签页的历史列表供用户快速跳转。
- 每个标签页必须独立维护浏览历史栈（包含 URL、标题、时间戳和当前位置），标签页切换、新建和关闭时历史栈生命周期独立。
- 新建标签页在空白页（`about:blank`）状态下必须在浏览器区域展示全局浏览历史面板：初始加载最近 20 条记录，支持无限滚动向下加载更多，直到全部历史加载完毕。
- 全局浏览历史必须持久化到 `~/.tiangong/browser-history.json`，应用重启后保留；每个标签页的导航历史仅存在于内存中，随标签页关闭而清除。
- 全局历史记录上限 1000 条，超出时自动保留最近的 800 条。
- 历史记录必须可通过 Tauri Command 导出给 Agent 作为参考上下文使用。

#### 工作区统一标签页
- Desktop 工作区面板必须使用统一 Tab 容器承载浏览器和终端，浏览器 Tab 与终端 Tab 可以在同一个 Tab 栏混排，并通过类型图标区分。
- 每个对话会话必须持久化自己的工作区 Tab 列表和当前活跃 Tab，切换会话时恢复该会话的浏览器/终端 Tab 集合。
- 会话 Tab 只持久化可序列化元数据，不持久化 PTY 进程或 WebView 实例；恢复会话时按记录懒创建运行实例。
- 终端必须支持同一会话内多个独立 Tab，每个终端 Tab 拥有独立 PTY、cwd 和持久化日志。
- 浏览器会话切换时必须关闭旧会话的 WebView 状态，并按新会话 Tab 记录恢复浏览器 Tab 元数据与活跃 Tab。
- `run_command` 和 `run_shell` 在 Desktop 模式下必须通过终端执行；执行前检测当前会话的终端状态，优先选择已打开且空闲的终端执行。
- 当当前会话没有可用终端，或所有已打开终端都处于繁忙状态时，系统必须自动创建新终端执行本次命令。
- 当命令因所有旧终端繁忙而在新终端中执行时，工具结果必须明确告知 Agent 本次使用了新开启的终端，而不是复用旧终端。
- Agent 自动调用 `run_command` 或 `run_shell` 时不得主动打开或切换到工作区终端面板；命令仍必须通过同一套插件终端 Tab 执行，终端 Tab 可在面板未展开时先创建或复用，并在创建或选中时立即写入当前会话的工作区 Tab 列表，用户主动打开终端面板后应能直接查看对应终端。
- `terminal_send` 作为 Agent 与已有终端交互进程协作的入口，应写入当前选中的可用终端。
- 普通命令执行默认使用 `run_command` / `run_shell` 前台终端链路，不得因为命令可能耗时就默认转为后台 task；后台 task 仅用于用户明确要求后台、不阻塞、并行执行、持续运行、服务监听或需要管理已有后台任务的场景。
- 命令交互态判断应优先依赖终端运行时信号（未返回结束标记、输出稳定、终端状态），避免通过固定程序名黑名单或提示词关键词表长期堆叠针对性规则；模型通过 `interactive` 参数表达交互意图，后端仅做通用兜底和状态协作。

#### Skill 文件系统注册表
- Skill 注册事实源必须从 `skills.json.installed[]` / `skills-lock.json` 迁移到 `~/.tiangong/skills/<id>/` 目录存在性。
- Skill 的 `id` 必须作为稳定机器标识用于目录、引用和审计；`name` 仅作为展示名称，不参与寻址。
- Skill 启停状态必须由 `skills/<id>/skill.toml` 中的 `available` 字段表达；字段缺失时默认可用。
- `SKILL.md` 不允许在启动期全量读入系统提示词，必须在激活、详情查看或检索命中时按需加载。
- 用户手动拷贝或删除 `skills/<id>/` 后，下一次扫描、刷新或激活必须能感知，无需重启应用。
- Skill 文件系统注册表必须保留 MCP 侧 `mcp-lock.json` 引用计数机制，不在本阶段重写 MCP 锁。
- 新机制不得继续读取或写入 `skills-lock.json`，该文件仅允许作为旧布局迁移输入被备份。
- 从 RFC-0003 旧布局 `skills/installed/<id>/<version>/` 到 RFC-0007 新布局必须提供自动迁移，失败时保留旧文件并可回退。
- Skill 管理 API 必须保持现有 `install/remove/enable/list/get` 行为兼容，同时新增 refresh/gc 能力用于重扫与孤儿清理。

#### 友好交互体验
- GUI 和 CLI 必须支持执行过程中实时展示中间推理/解释文本（边执行边解释）。
- 解释文本和工具调用必须独立展示：解释文本以流式非折叠方式展示，工具调用保持现有折叠展示方式。
- GUI 必须将连续工具调用合并为单个默认收缩分组，并在分组标题显示工具调用总次数和成功/失败统计。
- GUI 消息展示必须去掉用户和助手头像图标。
- GUI 消息文本必须去掉气泡边框和背景色，采用简洁无装饰风格。
- CLI 必须从阻塞等待改为实时流式展示：思考过程、工具调用摘要、最终回复均实时输出。
- Desktop 模式下，非当前查看会话在后台完成或失败时必须通过系统通知提醒用户；通知主链路应使用 Tauri 原生通知能力，权限拒绝或发送失败不得影响对话执行与快照同步。macOS 下通知点击或“显示”动作应尽量聚焦桌面端主窗口。
- Desktop 模式下，后台会话进入工具审批状态时应通过系统通知提醒用户；macOS 首期应支持在通知中快速同意或拒绝审批，其他平台允许降级为仅提醒用户回到应用处理。设计说明见 `docs/rfc/0010-desktop-background-notification.md`。
- Desktop 对话栏在用户向上滚动离开底部时，必须在右下角浮动展示"滚动到上一条用户提问"、"滚动到下一条用户提问"与"滚动到底部"三个按钮；用户回到底部后按钮自动隐藏。
- "滚动到上一条用户提问"按钮点击后将对话栏平滑滚动到当前可见区域之上的最近一条用户消息分组并对齐到视口顶部，已处于第一条用户提问时保持不动；"滚动到下一条用户提问"按钮点击后将对话栏平滑滚动到当前可见区域之下的最近一条用户消息分组并对齐到视口顶部，已处于最后一条用户提问时保持不动；"滚动到底部"按钮点击后将对话栏平滑滚动到最新消息位置。
- 用户主动向上浏览历史消息时，流式输出与新消息到达不得自动将视图强制拉回底部，必须保留用户当前位置；用户在底部时仍自动跟随新输出。用户主动发送新消息后默认跟随到底部。
- Desktop 对话栏在用户离开底部时，必须在右侧浮动展示用户提问导航轨道：每条用户提问对应一个定位点，定位点按等间距垂直排列；鼠标 hover 时以浮层显示该提问的前 15 个字符（超出尾部补 `...`）；点击定位点平滑滚动到对应提问并对齐视口顶部；当前视口顶部所在提问对应的定位点高亮激活；用户回到底部后轨道自动隐藏。

#### LLM 请求容错
- Agent 主循环的 LLM 流式生成不得设置整体生成超时；只要连接未断开、模型未返回结束事件且用户未主动取消，就必须持续等待模型输出。
- Agent 主循环调用主模型生成回复或工具调用时，默认 `max_tokens` 必须设置为高上限 `1024 * 1024`，避免兼容端点因缺少必填字段失败，同时降低长思考或复杂任务被输出上限截断的概率。
- LLM 请求遇到速率限制（429）、服务端错误（5xx）或超时时，必须自动重试。
- 默认最大重试 3 次，采用指数退避策略（初始间隔 1 秒，倍率 ×2）。
- 每次重试必须通过 `tracing::warn!` 记录日志（含重试次数、错误原因、等待时间）。
- 非可重试错误（如 401 认证失败、400 参数错误）不进行重试，直接返回错误。
- 重试逻辑必须覆盖所有 LLM 调用方法（complete / complete_stream / complete_with_functions / complete_with_functions_stream / complete_lite）。

#### 自动化触发层
- 必须支持定时任务（Cron），通过 `tiangong-scheduler` 独立 crate 管理，使用 JSON 文件存储（`~/.tiangong/scheduler/`）。
- 定时任务必须复用 silent 框架内置的 Scheduler 模块，Server 启动时自动恢复已启用的 Cron Job。
- 每个 Job 必须支持关联已有 session 复用上下文；未指定时自动创建新 session。
- 触发时必须构造结构化消息（任务名称 + 描述 + payload），通过 RuntimeEngine 执行链路处理，结果由 LLM 通过 MCP/Skill 决定如何处理，不强投递到 IM 通道。
- 必须记录每次执行的 JobRun（状态、开始时间、结束时间、结果摘要），支持通过 API 查询执行历史。
- Webhook 是 Server 内置的 HTTP 触发能力，独立于定时任务，拥有独立的 model 和 JSON 存储（`~/.tiangong/webhooks/`）。
- Webhook 必须提供无需认证的外部触发端点（`/api/v1/webhooks/:id/invoke`），支持可选的签名验证（X-Webhook-Signature）。
- Webhook 必须同时提供需认证的管理端点（CRUD + 手动触发 + 执行历史查询）。
- Job 和 Webhook 的执行逻辑必须共享同一个通用 executor，避免重复实现。
- Polling 触发器暂不实现，后续根据实际需求评估。

### Should

- Server 模式应支持 CORS 配置，方便 Web 前端调用。
- 应实现事件总线（EventBus）实现各层解耦通信。
- 应支持配置热重载，修改配置无需重启。
- 多媒体生成应支持 Stable Diffusion / Flux 等开源模型后端。
- Agent 层应集成 MediaAgent，支持在对话中自然触发图片/视频生成。
- 应支持 Docker 部署，提供 Dockerfile 和 docker-compose 模板。

### 非目标（当前阶段不做）

- 多用户权限与租户隔离体系。
- 商业化支付与计费。
- 公共 Skill 市场与评分系统。
- 微信接入（API 限制大，后续单独评估）。
- 多用户权限与租户隔离。
- 商业化计费。
- P2P 分发与去中心化网络。

## 参考

- `README.md`
- `PLAN.md`
- `TODO.md`
- `docs/rfc/0004-full-stack-agent-platform.md`
