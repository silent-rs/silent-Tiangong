# 当前任务：修复沙箱 stdio 中止阻塞（P0）

分支：`fix/sandbox-stdio-cancellation`。只创建一个 PR，以下任务全部完成并通过验收前不合并。

- [x] 从最新 `main` 创建独立分支。
- [x] 更新 `docs/requirements.md`、`PLAN.md` 和 `TODO.md`，登记请求级取消、并发处理、进程清理与响应时间要求。
- [x] stdio 普通请求并发分发并设置上限；停止请求绕过上限，主循环不等待长任务。
- [x] 按请求编号取消，建立会话、工具调用和 sidecar 请求关联；只取消目标任务。
- [ ] terminal、command 及其他子进程插件收到取消后清理目标进程树并释放占用。
- [x] 活跃任务中止立即投递；仅发送准备阶段等待发送完成后重试。
- [x] 思考强度只更新当前会话，不刷新其他 Core/插件，不跨等待持有会话锁。
- [x] 长 sidecar 调用移出界面命令处理线程。
- [x] 前端停止后立即显示“正在取消”，后端超时显示仍在运行与重试提示，真实结束前不显示空闲。
- [x] 同步 Rust sidecar 公共库、Node SDK 与官方 sidecar 制品版本；旧协议明确要求升级且不降级。
- [x] 验证 stdio 往返、崩溃重启、协议拒绝、Rust/Node 构建与当前平台进程生命周期；并发中止、跨会话与连续取消由三平台 CI 继续覆盖。
- [ ] 完成 Linux、macOS、Windows 检查、Rust 检查和前端 `yarn build`，创建单一 PR（不合并）。

完成标准：取消控制帧 100ms 内投递、后端 2 秒内确认接纳，正常目标进程在确认后 2 秒内退出；并发长任务下目标取消不影响其他请求或会话，后台运行状态始终真实，三平台与本地检查全部通过。

# 当前任务：Sandbox 通用化与直接命令行策略（feature/sandbox-direct-storage）

- [x] 从最新 main 创建独立分支。
- [x] 增加 `<storage>/sandbox/tiangong-sandbox[.exe]` 直存路径解析。
- [x] 拒绝程序或签名符号链接。
- [x] 增加只使用 Sandbox 独立官方公钥的生产验签与自检入口。
- [x] 完成 issue #458 P1 通用化和宿主自选安装目录。
- [x] `run` 支持直接命令行策略参数，并与 `--policy` 文件形式互斥。
- [x] 覆盖重复路径参数、网络、资源限制、非法值和兼容文件形式。
- [x] 完成格式、构建、测试、实际 CLI 验证与严格 lint；交付审查通过后提交。

完成标准：Sandbox crate 由宿主决定安装目录，提供策略文件与直接命令行参数两种执行方式，并保持官方根唯一的天工生产验证能力；不迁入 App 前端、Tauri 接线、Sidecar 或终端代码。

## Sandbox 自管理与 0.1.0 首版发布（feature/sandbox-self-management）

- [x] 从最新 main 创建独立分支，不迁入 App 前端、Tauri 设置或宿主更新器。
- [x] 迁移 macOS 26、自检与 `.git` 可写等纯 Sandbox 修复。
- [x] 新增 `check-update` / `update` 自管理命令。
- [x] 内置官方公钥、HTTPS、大小上限、SHA-256、官方验签和候选自检门禁。
- [x] 增加同目录候选落位、程序与签名成对替换、跨进程文件锁。
- [x] 发布链增加全局串行、主线来源、线上版本单调和四平台最终制品自检。
- [x] 增加自升级正向闭环及负面单元测试。
- [ ] 合并 main 后创建并推送 `sandbox/v0.1.0`。
- [ ] 确认 GitHub 官方签名与 OSS Secrets，完成四平台首版发布。
- [ ] 公开回读 latest.json 与四平台制品，并在干净存储根验证自升级命令。

完成标准：0.1.0 可由官方发布链安全投放；安装后的 Sandbox 能独立检查和升级，宿主后续只调用其命令。详见 `docs/sandbox-self-management.md`。

# 当前任务

## 工具执行超时统一归 Core（已取消）

- [x] 方案已取消并整体回退（提交 9fd38cb2）：保持 Core 不设工具超时、插件自行决定的既有架构。留档见 `docs/plugin-harness/optimization-plan.md` 第 3 节。

## 消息列表后台悬停导航（macOS，已完成待审查）

需求：docs/requirements.md「消息列表后台悬停导航（macOS）」。分支 `fix/message-list-inactive-scroll`，提交 5b94a1ae / e608f2b3 / 27c85ac4 / 2d0a9314 / 251ca788（未推送）。

- [x] 宿主轮询全局鼠标（inactive_hover 模块，窗口未激活且鼠标在页面内时下发页面视口坐标事件）。
- [x] MessageList 依据导航热区矩形以后台悬停状态唤出刻度尺与按钮组；窗口激活交还 CSS hover。
- [x] 坐标改为 macOS 原生窗口/视图转换（NSEvent→NSWindow→WebView 视图；删显示器枚举与 scale 换算；AppKit 全在主线程，in_flight+超时防堆积）。
- [x] 后台悬停驱动横条变宽/高亮与问答预览卡（externalPointer 转发，复用真实指针路径）。
- [x] 后台首击定位（宿主补发点击位置 + externalClick/卡片跳转；acceptsFirstMouse 不可靠不用）。
- [x] 后台热区滚轮接管：实测应用层三层均不可行，按决策放弃（记入需求非目标）。
- [x] cargo fmt / check / clippy -D warnings 与前端 build 通过。

## 方向调整：Creator 回归可选官方插件 + 官方解释器分发

- [x] 任务 A：撤回内置交付——删除启动部署/打包资源/构建链接入/专用依赖与测试，恢复可选插件语义。
- [x] 任务 B：官方解释器 Sidecar 分发——签名锚定内容清单、发布端归档产物（tar.zst）、目录 any 平台条目、下载解包安装链、运行时官方签名放行（与本地信任三分支不混用）。
- [x] 任务 C（本地部分）：正式产物完整（归档/签名/any 条目/幂等）、目录安装全链路（发现→下载→解包→验签→安装→加载→sidecar 调用）与卸载语义真实测试通过；三平台与目录升级（0.1.0→0.2.0）检查留 CI/发布时执行。


### 审查修复（签名信任边界与发布链）

- [x] 官方签名真实覆盖解释器插件全树：`content_manifest` 签名条目接入验签流程（条目必填、路径锁定 content-manifest.json、哈希校验 + 内容清单双向全树校验）；native/纯 UI 形态携带该条目或解释器携带二进制条目均拒绝；篡改 sidecar/模板/页面/清单/额外文件全部拒绝（负面测试矩阵）。
- [x] 官方签名与本地信任互斥：同时携带两种信任来源拒绝启动（测试覆盖）。
- [x] 市场支持判断认识 `sidecars.any`（平台无关归档全平台可装）；目录规则拒绝 any 与平台条目混用、any 缺签名条目。
- [x] 归档解包加固：条目类型白名单（仅普通文件/目录，拒绝符号链接/硬链接/设备/FIFO）、条目数/单文件/总量/路径长度上限。
- [x] 下载链防归档偷换身份：解包到独立目录并与目录清单下载的 plugin.json 逐字节比对一致后才合并。
- [x] 发布工作流适配解释器形态：resolve-plugin 输出 release_kind，解释器插件走单一 Fresh Checkout 完整构建任务（any 片段），合并期望平台与签名数量按形态分派；xtask 签名改为 minisign 库直签（兼容 tauri signer 私钥，去 tauri-cli 依赖）。
- [x] 官方目录安装全链路改为普通 CI 必跑：e2e 迁入 runtime 集成测试（环境门控 + 产物/密钥由 CI 提供：测试密钥自动生成 + 本地 http 目录服务），卸载语义测试同源归档化。

## 统一签名信任与三方插件导入（已完成）

需求：docs/requirements.md「解释器形态 Sidecar 与统一签名信任」。三类信任根（官方内置 / 本机用户密钥 / 导入三方公钥）共用一条签名验证路径，创作链免交互，三方经本地导入。

- [x] 信任根模块：用户密钥对惰性生成（keys 目录、权限收紧）、三方公钥登记表（导入 / 指纹 / 移除）、发布者 → 公钥路由（保留测试用公钥环境覆盖，仅作用于官方形态）。
- [x] 验证合并：verify_signed_release 按 publisher 路由信任根；publisher 格式校验放宽；未导入发布者给出明确指引。
- [x] 创作链自动签名：plugin_dev 安装时宿主以用户密钥签发签名清单（发布者 local）并走签名验证安装；移除创作链安装确认（含宿主接入与前端弹窗清理）。
- [x] 三方导入通道：本地目录 / 归档导入统一走签名验证；导入时展示权限清单（敏感权限标记）确认一次。
- [x] 遗留兼容：存量 local-trust 插件只读可用；新装不再落锚；签名与遗留标记同存拒绝（保留）。
- [x] 前端设置：密钥管理界面（三方公钥导入 + 指纹展示 + 移除；用户密钥指纹展示）。
- [x] 测试：三方导入验签 / 未导入拒绝 / 移除后失效；创作链自动签名安装与调用；存量 local-trust 回归；篡改矩阵回归。

### 审查修复：签名授权绑定到受信 Creator（身份不可自报）

- [x] 受信安装方注入：自动签名安装只对官方签名的固定 Plugin Creator 放行（宿主注入策略、runtime 插件中立、未注入 fail-closed）；其他声明 `plugin-dev.use` 的插件不可触发用户密钥签名。
- [x] 构建产物溯源：宿主以 sidecar 结果观察者登记官方 Creator 默认开发根下的成功构建（通用机制、不解析业务语义），install 只接受有登记的项目；非默认根（root 覆盖）不产生安装资格。
- [x] 信任存储加固：用户密钥与登记表原子落盘（临时文件 + rename），密钥生成与登记表读写串行锁（防双生成竞态与并发丢更新）。
- [x] 测试：安装授权四场景（fail-closed / 非受信插件 / 缺登记 / 受信齐全）、sidecar 结果观察者触发。
- [x] 构建登记绑定产物指纹（内容清单整体哈希）：install 与暂存副本比对，构建后替换产物拒绝；仅 `ok == true` 登记失败撤销；安装成功消费登记（一次构建一次安装）。
- [x] 用户密钥崩溃残缺自愈（公钥从私钥重建、私钥损坏明确报错）；私钥临时文件 0600 直设（无宽权限窗口）。
- [x] Creator 工具与模板文档描述同步为「自动签名免确认」（plugin.json 工具描述 / README / 各模板）。
- [x] 官方信任根唯一化：删除 TIANGONG_PLUGIN_PUBKEY_B64 覆盖通道（运行时与部署守卫均直绑内置公钥）；验签拆显式公钥底层 + 发布者路由两层；负面测试（测试密钥冒官方被拒、环境变量残留不影响）；发布 CI 增加 verify-official-release 强制校验（私钥与内置公钥不匹配即中止）；官方目录 e2e 重写为「冒官方被拒 + 三方签名完整链路 + 卸载」。
- [x] 构建指纹核验扩展到全部 Creator 模板产物（纯 UI / 工具 / sidecar），含纯 UI 篡改拒绝测试。
- [x] 文档全仓清理（官方信任根不可配置表述、历史方案废弃标注）。
- 已知边界（记录于需求文档）：单 WebView 下主窗口恶意脚本钩取调用参数、devkit 依赖供应链——多 WebView 演进与依赖治理为后续。

## 信任体系决策收尾（2026-08-27，#447/#448 已合并）

- [x] feature/plugin-trust-model 早期独立尝试（手工信任登记 + unsafe_mode）评估：核心语义已被统一签名信任完全覆盖且更强，分支删除。
- [x] unsafe_mode（RFC 0017 L4 放开开关）明确不开发：RFC 0017 修订标注（docs/rfc-0017-plugin-trust-and-sandbox 分支已提交推送）+ 需求文档非目标记录；替代路径为用户密钥签名 / 三方公钥导入 / 测试密钥局部验证。

## 解释器形态 Sidecar 与本地信任（feature/sidecar-interpreter-runtime）

> 历史方案章节（原生确认与 local-trust 锚已被「统一签名信任」取代，见上方进行中章节；条目保留作演进记录）。

范围：插件清单支持结构化声明解释器 sidecar（`sidecar.runtime` node/python 枚举 + entry），宿主白名单分派、强制 stdio 通道；自建插件经插件创作链原生确认建立本地信任（内容清单哈希落锚与篡改复核）；交付 Node 协议库（plugins/sdk-sidecar）与 node-sidecar 创作模板。前置：Sidecar stdio 传输铺路分支。Python 协议库与模板、解释器沙箱档为后续。

- [x] 清单 schema：SidecarRuntime 枚举（native/node/python）、runtime/entry/args 字段与互斥校验（entry 须为子目录内安全相对路径）。
- [x] 本地信任：安装确认后落 local-trust.json（内容清单整体哈希锚）；启动门槛三分支（官方签名 / 本地信任解释器 / 拒绝）；连接与 spawn 前复核全树哈希。
- [x] 宿主分派：解释器程序解析（PATH + TIANGONG_NODE_PATH/TIANGONG_PYTHON_PATH 覆盖）、stdio spawn 以「解释器 + entry + args」启动、通道强制 stdio。
- [x] Node 协议库 plugins/sdk-sidecar：零依赖 .mjs，帧协议/认证/握手/进度/通知与宿主逐字段对齐。
- [x] node-sidecar 模板（零构建工具页 + sidecar 入口 + vendor 协议库）与 devkit validate 的 runtime 枚举更新。
- [x] 真实链路验证：node e2e（往返/崩溃重启/篡改拒绝）、安装链集成测试（本地信任安装→真实调用→篡改拒绝、无信任拒绝）、devkit init/validate/build、模板入口真实帧协议握手与调用。
- [x] 运行格式、workspace 构建、测试与 clippy 全套；已提交分支（推送与 PR 待定，PR 链式依赖铺路分支先合）。
- [x] GUI 环境解释器探测补齐（fix/interpreter-discovery）：PATH 未命中后枚举常见安装位置——nvm/asdf/pyenv 版本目录从新到旧、volta、Homebrew（/opt/homebrew/bin、/usr/local/bin）、Linux 系统路径、Windows Program Files；全部未命中时报错文案引导用户在会话中对助手说「帮我安装 Node.js / 帮我安装 Python」快速安装。背景：App 由 launchd 启动不执行 shell 初始化，nvm/Homebrew 安装的 node 不在进程 PATH，插件管理页出现「未找到 Node sidecar 所需的解释器程序」。
- [x] 入口解释器环境注入（fix/interpreter-discovery 续）：`registry::ensure_interpreter_env` 由 tiangong-app main() 在任何后台线程启动前调用——探测结果写入 TIANGONG_NODE_PATH/TIANGONG_PYTHON_PATH（外部显式设置不覆盖），并把解释器所在目录前置进 PATH（已含则跳过），使命令通道子进程（agent 执行 yarn/npx、devkit 构建链）全树可用；未探测到时不改动留待运行时引导。约束：std::env::set_var 非线程安全，严格限定线程池启动前调用。
- [x] 审查修复四项（fix/interpreter-discovery）：显式指定解释器时同样前置其目录进 PATH（仅路径有效时）；Windows 探测补全（user_home_dir 复用、nvm-windows/Volta/scoop、Python 官方安装器用户级与系统级版本化目录、.exe 后缀、Python3xx 版本排序）；测试 HOME 覆盖改 EnvRestore 守卫可靠恢复；PATH 测试改 join/split_paths 平台无关构造。
- [x] 探测环境快照重构（fix/interpreter-discovery 续，二轮审查）：新增 InterpreterEnv——生产 from_process 一次读取、测试注入伪造值，探测零环境污染，并行测试互不干扰；采纳分层回退（安装工具根目录 NVM_HOME/NVM_SYMLINK/VOLTA_HOME/SCOOP/ChocolateyInstall/PYENV_ROOT/NVM_DIR/ASDF_DATA_DIR → 版本管理器目录从新到旧 → 系统标准位置，工具变量缺失不判定不存在）；修复 Windows 系统级 Python 路径（%ProgramFiles%\Python3xx 直接位于 Program Files 下，经变量推导）并补 pyenv-win；Volta 三平台统一（修复 macOS 遗漏）。
- [x] 探测迁出 registry（fix/interpreter-discovery 续）：registry.rs 超 3200 行，解释器探测整体迁至 src/interpreter_env.rs（含测试）；lib.rs 顶层导出 ensure_interpreter_env，入口调用路径随之上移；user_home_dir 归属新模块（pub(crate)，registry 复用）。
- [x] 探测完全快照驱动（fix/interpreter-discovery 续，三轮审查）：PATH 与系统标准位置纳入 InterpreterEnv 快照，探测结果只由快照与文件系统决定；显式工具根（NVM_DIR/ASDF_DATA_DIR/PYENV_ROOT/VOLTA_HOME/NVM_HOME/scoop）直接生效不依赖 HOME；pyenv-win 双布局兼容；Windows 标准目录严格环境变量驱动（ProgramFiles 不兜底写死、ChocolateyInstall 经 ProgramData 推导）；测试补齐 PATH 优先、空 PATH 回退、全空返回 None、显式根 × HOME=None、Windows 系统 Python 真实目录排序等场景。
- [x] 发现两层化：应用级缓存 + 平台策略（fix/interpreter-discovery 续，四轮方案）：InterpreterKind/Source/CachedInterpreter 全局缓存，统一 resolve_interpreter 入口（命中仅 is_file 校验，失效重探写回），registry/入口注入共用；显式无效不回退、应用注入值失效不再误判为显式（source 区分）；invalidate_if_matches 仅清匹配路径，stdio 进程创建失败时失效重解析重试一次；Windows 改纯环境变量固定路径（不枚举版本目录，NVM_HOME 不参与），Unix 版本枚举仅作缓存未命中慢路径；缓存测试含计数器验证不重复探测。
- [x] 恢复链路修复（fix/interpreter-discovery 续，五轮审查）：注入标记独立于缓存存活（残留环境变量不误判为用户显式，真实链路验证抓出读写 static 不共享缺陷并修复）；InterpreterLaunch 删除 program 字段，stdio 每次启动经缓存入口取最新路径；SpawnAttemptError 分类——仅进程创建失败触发失效重试，前置错误不清缓存；per-kind 探测锁 + 双检，并发首次解析只探测一次。
- [x] 失败恢复排除坏路径并同步环境（fix/interpreter-discovery 续，六轮审查）：probe 系列支持排除路径（全部候选来源统一过滤"文件仍在但不可执行"的旧解释器）；recover_interpreter_after_spawn_failure 原子接口——缓存匹配失效 → 排除重探（锁内双检）→ 写缓存，应用注入场景同步 TIANGONG_*_PATH 与 PATH 前置，用户显式失败不静默替换；stdio 重试改走本接口；e2e 测试清理未使用 node 参数（严格检查通过）。
- [x] 运行期环境纪律收紧（fix/interpreter-discovery 续）：全局环境变量仅作启动输入与初始继承，运行期权威来源为解释器缓存——recover 删除运行时 set_var，仅失效→排除重探→更新缓存；新增 child_env_overrides 从缓存派生子进程覆盖（TIANGONG_*_PATH + 前置解释器目录的 PATH），stdio 创建 sidecar 时经 Command::env 单独注入，恢复后的新路径由此传导给子进程树；真实链路验证宿主环境恢复前后完全不变。
- [x] 恢复链路收尾三项（fix/interpreter-discovery 续，八轮审查）：并发恢复先取探测锁再判缓存（后来者复用他方恢复结果，判断/失效/重探全在锁内）；二次失败返回真实错误且若是解释器创建失败则清掉刚恢复的坏缓存（不做第三次恢复）；子进程 PATH 改强制前置（移除后前置）——恢复后新解释器目录不再因已在 PATH 中而排在坏目录之后；启动阶段保持普通前置。
- [x] 审查通过后收尾（fix/interpreter-discovery 续，九轮）：恢复接口注释与并发复用行为同步；补二次恢复复用（探测计数 1）、强制前置居中目录、坏目录在前完整恢复场景三测试——并修正上轮批量替换静默失败导致测试未落盘的出入；计数器去除多余 Arc（严格检查零警告）。

完成标准：用户经插件创作链可安装带常驻 node sidecar 的自建插件并真实调用；解释器 sidecar 仅 stdio、仅本地信任放行、篡改即拒；现有官方插件（无 runtime 字段）行为与 main 一致；官方签名（解释器形态锚定内容清单）与本地信任不混用，同存即拒。

## Command 沙箱执行套壳（feature/sandbox-execution）

范围：只交付 command 通道的宿主强制沙箱套壳。插件信任模式、工作区快照恢复、无沙箱审批升级、其他 sidecar 迁移和 Launcher 在线更新均由独立分支处理。

- [x] 当前分支相对 `main` 移除信任、快照、审批升级、其他插件迁移和插件目录改动。
- [x] 修复 Launcher 的平台条件与自检清理边界，保证不会递归删除系统临时目录。
- [x] 将本次执行的专用临时目录加入可写策略，并设置 `TMPDIR`、`TMP`、`TEMP`。
- [x] 删除进程级全局工作区注册表，由本次工具调用上下文直接传入权威工作区。
- [x] Launcher 拒绝符号链接，并把目标限制在宿主声明的已安装 command 插件目录。
- [x] Unix 使用独立进程组清理调用进程，Windows 使用 Job Object；Windows 完整文件隔离不可用时明确拒绝执行。
- [x] 用跨平台工具替换 Bash 打包脚本，按 Tauri 实际目标三元组复制 Launcher 制品。
- [x] 移除当前分支未完成的 Launcher 在线版本选择预留，只保留随 App 发布的固定版。
- [x] 增加真实 Launcher 集成验证，覆盖工作区内写入、工作区外拒绝和专用临时目录。
- [x] 增加独立 Sandbox CI，隔离 App/Plugin CI；Linux、macOS、Windows x86_64 原生运行器分别验证平台能力、进程清理与打包制品。
- [x] 运行格式检查、workspace 构建检查、相关 clippy、测试和前端构建。
- [x] 使用临时伪造的 HOME、系统配置和工作区执行危险命令矩阵，真实验证敏感读取、工作区外删除、网络外传、后台持久化、`.git` 篡改和配置写入均失败，绝不触碰运行器真实文件。
- [x] 在 Launcher 自检报告中加入网络阻断和敏感读取阻断结果，并在 Linux 与可运行 Seatbelt 的 macOS 环境断言真实隔离。
- [x] Linux CI 单独运行真实 Launcher 的后台进程树清理用例；macOS 托管环境尽力执行，无法嵌套 Seatbelt 时明确记录受限原因。
- [x] 打包后的 Launcher 必须是非符号链接的普通可执行文件，版本与应用一致，并实际运行自检；三平台验证目标名称和权限。
- [x] 增加完整 Tauri bundle 验证，检查安装包中包含正确目标的 `tiangong-sandbox`，且解包后的 Launcher 路径可解析。
- [x] 增加同一策略跨平台序列化与平台策略生成的一致性验证。
- [x] 增加宿主文件描述符或 Windows 句柄泄漏验证，确保多次启动和停止后资源数量回落到允许误差内。
- [x] 增加并发启动、并发取消和并发停止验证，覆盖至少 10 个相互隔离的 command 调用。
- [x] 为单次 command 执行增加内存与进程数量限制，并分别验证超限进程被拒绝或终止。
- [x] 增加命令超时后整棵进程树强制终止的真实链路验证。
- [x] 清除 `LD_PRELOAD`、`LD_LIBRARY_PATH` 及对应平台的动态加载环境变量，并验证 `TMPDIR`、`TMP`、`TEMP` 只指向本次执行目录。
- [x] 增加工作区符号链接逃逸、目标路径穿越和 `.git` 元数据保护验证。
- [x] CI 失败时输出 Launcher、sidecar 的实际日志、命令输出、退出码和平台自检报告，不输出凭据或真实敏感文件内容。
- [x] macOS 在宿主被强制终止且业务请求仍在运行时，独立监视宿主存活并立即终止 sidecar 进程组。
- [x] 增加 macOS 原生真实用例：请求执行中终止宿主，断言 sidecar、命令和后台子进程均及时退出。
- [x] 完成 macOS 修复后的格式、构建、相关测试与 Sandbox CI，提交并推送当前分支。

完成标准：command 每次调用只获得所属会话工作区与本次临时目录的普通文件写权限，不得访问网络、用户凭据或修改工作区 `.git` 元数据；其他会话无法改变该权限；危险环境变量、符号链接和路径穿越无法逃逸；取消、停止、超时或资源超限后不遗留进程与宿主资源；Launcher 与完整 App 打包制品在支持的平台可构建、可识别并实际验证；所有相关检查通过。

## 待办（用户决策，待集成分支处理时执行）：移除随 App 分发

用户已决策：沙箱不随应用安装包分发，Launcher 唯一生产来源是官方在线更新链。集成分支冻结期间不动，待处理集成时执行以下拆除（2026-08-28 已在临时分支验证过一轮方向，因误动集成分支被回滚，留档避免重探索）：

- tauri.conf.json 删除 externalBin 两项；App CI 四任务的密钥与 prepare-sandbox 步骤删除；xtask prepare-sandbox 命令删除。
- Sandbox CI 完整版删除三平台"打包形态制品"验证与 bundle 任务（注意 rebase core 时需以 core 新版为基叠加集成测试，勿用旧命名基底）。
- 无 Launcher 时解析返回 None、命令执行 fail-closed，首次由后台更新器在线获取。
- core 侧已完成：解析注释已对齐在线获取语义（builtin 为可选同目录来源，版本基准为编译期 crate 版本）。

## 分支重组（2026-08-28）：沙箱独立开发与 App 集成拆分

按用户决策，沙箱功能暂不直接集成进 App，历史四层分支（execution → toggle → launcher-signing → launcher-update，全部工作）重组为两个分支：

- **feature/sandbox-core**（基于最新 main）：沙箱独立组件。crates/tiangong-sandbox 全部能力（策略编译、Launcher、自检、签名、独立版本序列、在线更新解析与发布链 publish-sandbox）+ 独立 Sandbox CI（三平台 crate 验证 + 发布制品断言）。合入 main 不改变 App 行为（无任何接线）。
- **feature/sandbox-app-integration**（基于 sandbox-core）：全部 App 接线——宿主强制路由（stdio/Launcher/验签/资源上限）、设置开关与状态图标、command sidecar 适配、externalBin 打包、App CI prepare-sandbox 与 Sandbox CI 完整版、Launcher 更新器后台任务。等用户决定集成时机再合。

拆分验证：集成分支与原 launcher-update 最终态的功能文件零差异（仅版本基线 0.15.3 与 main 的 CI 引号修复）。旧分支（execution/toggle/signing/update）与 PR #441/#452 待处置。

## 沙箱 Launcher 直存与设置 Modal 调整（已完成）

- [x] 删除宿主 active/pending、版本仓库与重启激活路径，Sandbox 和签名直接存放在 `sandbox/`。
- [x] 首次安装和手动更新复用 Sandbox 自管理安装能力，宿主不重复实现下载、验签和替换。
- [x] 环境变量屏蔽清单使用摘要 + 管理 Modal，支持草稿、格式校验、大小写去重、取消与失败重试。
- [x] 修复终端工具事件跨会话抢占，宿主按 invocation.session_id 加载权威工作区。
- [x] 完成 Rust workspace、前端、终端插件构建和相关测试。

## 终端与命令 Git 凭据只读例外（feature/sandbox-app-integration）

- [x] 宿主策略仅为官方签名的 `terminal`、`command` 开放 `~/.ssh` 与 `~/.config/gh` 读取，其他插件继续禁读。
- [x] 两个目录继续进入写保护清单，不因读取例外获得写权限。
- [x] 仅为官方签名的 `terminal`、`command` 开放 `~/.cache` 读写，其他主目录写限制和其他插件策略不变。
- [x] 核对 `coding` 仅执行本地 Git 状态、差异与基线检查，不需要 SSH、GitHub CLI 或用户缓存权限，保持原策略。
- [x] 验证策略分流、读取例外、系统钥匙串、SSH 远端与缓存写入，并完成格式、相关测试、严格检查和 Rust workspace 编译。

完成标准：嵌入式终端和 command 沙箱内可使用 SSH、Git 远程操作与 GitHub CLI 所需配置并读写 `~/.cache`；其他 sidecar 仍无法读取这两个凭据目录或写入用户缓存，所有 sidecar 都不能写入两个凭据目录。

## Sandbox 首版发布兼容修复（feature/sandbox-app-integration）

- [x] 保持首版策略 Schema 为 2；新增兼容字段不单独提升策略版本，本地 0.1.0 通过重建和重签更新。
- [x] 本地 Launcher 完整有效时不访问远端首版清单；本地不存在或校验失败且远端明确 404 时保持沙箱不可用，但不阻塞进入应用。
- [x] 启动准备页一次重试只发起一次准备请求。
- [x] 验证重建后的本地 0.1.0 校验通过、启动不触发远端获取、无有效本地版本时 404 仍明确失败，并完成 Rust、前端构建检查。

完成标准：远端首版尚未发布时不误伤完整有效的本地沙箱；本地预发布 0.1.0 可通过重建重签接入新行为，不引入额外策略版本；没有任何有效沙箱时仍保持拒绝执行。

## Sandbox 直存布局与旧缓存修复（feature/sandbox-app-integration）

- [x] 当前生效的 Launcher 与签名固定存放到 `sandbox/tiangong-sandbox[.exe]` 和 `sandbox/tiangong-sandbox[.exe].sig`，不再从 `sandbox/versions/<版本>/` 解析。
- [x] 删除宿主 active/pending 与版本仓库更新器，首次安装和手动更新复用 Sandbox 自管理安装，程序与签名成对替换。
- [x] 成功安装直存文件后清理旧 active/pending、versions 与事务目录，旧缓存不再参与解析。
- [x] 同步 Launcher 定位、安装、状态、前端入口与验证；同时修复终端工具按 invocation.session_id 定向及宿主权威工作区解析。

完成标准：稳定状态下 `sandbox/` 根目录直接包含当前 Launcher 与签名；旧布局可自动迁移且不再参与解析；Sandbox 自管理安装可靠成对替换；嵌入式终端可正常使用 SSH 与 GitHub CLI。

## 沙箱 Launcher 在线发布与更新（feature/sandbox-launcher-update，基于 signing 分支）

范围：Launcher 独立版本序列 + 官方 OSS 在线清单 + 宿主后台更新 + 发布工作流。按用户修订：Launcher 版本与 App 解耦（独立序列），协议判定为"协议相等 + 策略向后兼容（清单 policy_schema_max ≥ 宿主值）"，避免升级被卡。

- [x] 协议常量归一：LAUNCHER_PROTOCOL_VERSION/POLICY_SCHEMA 移入 tiangong-sandbox 库层唯一权威，bin 与宿主 stdio 引用（消除硬编码人工同步）。
- [x] Launcher 独立版本序列（crate 0.1.0 起步）；CI 版本断言改为对 crate 版本。
- [x] 解析升级：最初使用 `sandbox/versions/<版本>/` + active 指针；该历史布局已由上方“Sandbox 直存布局与旧缓存修复”任务替代。
- [x] 更新器 launcher_update：清单判定（版本>内置、协议相等、策略兼容）、HTTPS+流式 SHA-256 下载、激活三重门禁（官方根验签——不接受用户根；真实自检退出 0 且自报版本/协议与清单一致；原子落位+active）。
- [x] 宿主接入：start_launcher_updater 启动后台检查一次，离线/失败记日志重试。
- [x] 发布链：xtask prepare-sandbox-release（构建+签名+平台片段，无私钥 fail-closed）+ publish-sandbox.yml（三平台矩阵、官方私钥 secret、清单三步写+回读校验+制品抽验，协议常量从源码提取）。
- [x] 测试：清单适用性矩阵、URL/校验和单测；本地 HTTP 目录服务集成（真实下载+校验和通过+非官方签名被官方根拒绝激活、active 未写入）。
- [x] fmt/check/clippy 零警告/相关测试/前端 219 项全绿。

已知边界：正向激活全链路（官方私钥签名制品→自检→激活）无法在无官方私钥的环境自动化，由发布 workflow 的制品抽验 + 官方环境联调覆盖；无运行时故障自动回退（自检门禁替代）、无手动更新 UI（后续按需）。

## 沙箱 Launcher 签名与验签（feature/sandbox-launcher-signing，基于 feature/sandbox-toggle）

范围：Launcher 复用插件 minisign 信任体系——打包签名、宿主启动前验签（官方内置公钥或本机用户密钥任一通过）、fail-closed。在线发布/更新策略明确后置。

- [x] 验签侧：signature.rs 公开 verify_launcher_signature（官方根→用户根）；stdio.rs 沙箱分支启动 Launcher 前逐次验签。
- [x] 签名侧：prepare-sandbox 构建后签名（无私钥 fail-closed 并给配置指引），签名文件按 externalBin triple 规则命名；新增 sign-sandbox 命令为本地 debug Launcher 重签。
- [x] 打包：externalBin 增加 tiangong-sandbox.sig 条目（tauri build script 已验证接受）。
- [x] 测试：fixture 自包含（静态测试密钥确定性签名 + 公钥写入 fixture 存储根）；验签单元用例覆盖通过/篡改/缺签名/不受信密钥四路径；ephemeral 真实链路隐式经验签。
- [x] CI：prepare-sandbox-key composite action（测试密钥注入），ci.yml 四任务 + sandbox-ci 三平台与 bundle 接入；bundle 验证三平台断言签名文件在包内。
- [x] 本地验证：私钥签名产物生成、无私钥拒绝、sign-sandbox、单元与集成测试、fmt/check/clippy/前端全绿；三平台真实拦截由 Sandbox CI 验证。

## 命令沙箱用户开关（feature/sandbox-toggle，基于 feature/sandbox-execution）

范围：设置中提供全局"命令沙箱"开关（默认开启）。关闭需警示确认；关闭期间输入区信任标识左侧常显红色"沙箱关"警告（点击跳转设置）；资源上限不依赖沙箱照常注入。附带修复两个主线问题：Launcher rlimit 的 Linux 编译错误（宏化平台类型）、command 按需形态 init 状态不跨请求（请求内权威上下文刷新，已 cherry-pick 回 execution 分支）。

- [x] 配置层：TiangongConfig/app.json 增 sandbox_disabled（serde default 兼容旧配置，默认开启不写字段）；try_sandbox_disabled 非恐慌读取。
- [x] 决策链：按需进程沙箱开关控制 terminal、command、解释器及清单声明的按需 sidecar；plugin-runtime 宿主执行层自主维护状态，不耦合 TiangongCore、TurnContext 或 Plugin trait；切换时停止现有按需进程并在下次操作按新状态重建，预加载常驻服务继续强制沙箱。
- [x] Tauri 命令 get/set_sandbox_disabled（照 default_trust_mode 模式，落盘+内存热更+状态同步）。
- [x] 前端：api/store 全局状态；设置页开关+关闭警示确认弹窗（destructive）；输入区信任标识左侧红色 Unlock"沙箱关"（仅关闭时显示，点击打开设置）。
- [x] 无沙箱直跑真实链路测试（开发机可真实执行，不依赖沙箱环境）。
- [x] fmt/check/clippy 零警告/相关测试/前端 build+test（219 项）全绿；需求文档本地同步（第 14 条与非目标衔接）。

完成标准：默认开启走沙箱行为不变；关闭后 command 真实直跑成功、资源上限保留、状态图标显示、警示日志记录；重新开启即恢复沙箱；旧配置文件兼容。

## 命令沙箱加固（fix/sandbox-hardening，基于 feature/sandbox-execution，已并入 PR #441）

范围：对照 octos 实现分析补齐命令沙箱的四个薄弱点。前置分支 feature/sandbox-execution（尚未合入 main）。

- [x] 敏感读取拒绝清单扩充：GPG/Kubernetes/Docker/Azure/GCP/GitHub CLI/.netrc 与天工数据根内 keys、trust.db、mcp.json、models.json、server.json、app.json；自检假凭据矩阵覆盖 GPG。
- [x] Unix 资源上限移入 Launcher 强制边界（exec 前 setrlimit 随继承传播，外层更严时继承现状）；SidecarConfig 暴露资源限制覆盖；真实链路验证 CPU 上限强制终止死循环。
- [x] 修复 macOS 命令启动失败：darwin setrlimit 对 RLIMIT_AS 一律 EINVAL（原 sidecar 层设置会使 macOS 全部命令 spawn 失败），内存/进程数上限改为 Linux-only，macOS 由命令超时与进程树清理兜底。
- [x] 环境注入防护对齐 octos：spawn 层与 runtime/file env 注入层拒绝 NODE_OPTIONS、PYTHONSTARTUP、PYTHONPATH、PERL5OPT、RUBYOPT、RUBYLIB、JAVA_TOOL_OPTIONS、ZDOTDIR；集成测试攻击载荷同步扩展。
- [x] Seatbelt SBPL 注入防护：策略路径含控制字符或非 UTF-8 时编译为拒绝一切 profile；Launcher 入口平台无关结构化拒绝；括号（macOS 合法文件名）不误伤。
- [x] fmt/clippy/check --workspace 与相关 crate 测试全绿；需求文档本地同步（第 10–13 条）。

已知边界：开发终端处于嵌套 Seatbelt 环境，真实拦截用例本地跳过（单元级与 CPU 上限用例真实执行），Seatbelt 真实拦截与 Linux 内存/进程上限由 Sandbox CI 三平台验证；未做项（Linux Landlock 回退、macOS setsid 进程逃逸验证、Windows 拒绝归因、读取白名单化、网络白名单）为后续独立任务。

## Sidecar stdio 传输铺路（feature/sidecar-stdio-transport）

范围：先行交付 stdio 传输层，为逐插件沙箱覆盖铺路。插件侧通用库增加 stdio 通道分发（宿主以环境变量选择，插件业务代码与清单零改动），宿主侧增加 stdio 连接与携带会话上下文的调用链；全部插件保持 TCP 通道不变，不含任何 OS 沙箱内容。

- [x] 插件侧通用库 stdio 通道：Auth 首帧、帧协议与 TCP 一致、stdin EOF 退出、进程组清理与 macOS 宿主监视。
- [x] 宿主侧 StdioSidecarConnection：懒启动、握手身份校验、请求往返、进度与通知转发、崩溃换代重启、宿主退出清理进程组。
- [x] 工具调用链携带宿主权威上下文（会话、调用标识、会话工作区），新增会话取消传播。
- [x] sidecar 连接表 trait 化；通道选择逻辑与策略表留在沙箱覆盖分支，本分支所有插件仍走 TCP。
- [x] stdio 端到端测试（往返、握手、崩溃重启、macOS 宿主崩溃清理）与 test-stdio 手动验证工具。
- [x] 运行格式、workspace 构建、相关测试与 clippy；test-stdio 工具完成真实握手、请求执行与宿主强杀后的进程组清理验证；已提交分支（推送与 PR 待定）。

完成标准：stdio 通道在通用库与宿主侧真实可用并通过端到端与手动验证；所有插件（含 command）行为与 main 一致；分支不包含 `tiangong-sandbox` 内容；插件业务代码与清单零改动。

## Sidecar 插件沙箱覆盖（feature/sandbox-plugin-coverage）

范围：当前任务。前置：Sidecar stdio 传输铺路分支已合入 main。逐个审计所有带 sidecar 的插件，由宿主、运行时和 Launcher 强制应用对应策略。不得根据插件名称直接套用统一策略；每个插件都要确认真实传输、文件目录、网络、平台接口、长连接和子进程需求。本分支不修改 `plugins/` 中的代码或清单，不增加兼容标记。

- [ ] 建立带 sidecar 插件清单，记录当前传输、实际读写目录、网络需求、平台接口、子进程和持续连接需求。
- [ ] 由宿主真实握手核对每个已发布插件制品的 stdio 支持，确定升级或明确拒绝策略；不向插件清单增加兼容标记。
- [ ] 扩展宿主策略与 Launcher 目标校验，使非 command 插件只能获得审计后声明的最小权限。
- [ ] 为无 PTY、无平台接口、无动态路径需求的插件应用宿主强制策略，并验证真实工具调用。
- [ ] 分别处理 mcp、scheduler 等网络或长连接插件，确保传输与联网策略不扩大权限。
- [ ] 分别处理 terminal、computer-use、screenshot-input、fs、memory 等特殊插件，保持现有能力并验证隔离边界。
- [x] 修复按需启动 sidecar 的工作区写权限：会话 handler 触发终端等连接时由宿主传入该会话的权威工作区并按工作区隔离；随应用持续运行的 sidecar 保持原有通用可写域。
- [x] 修复 macOS 26 PTY 放行规则的 SBPL 字符串错误，并恢复新版系统上的真实 Seatbelt 单元用例执行，避免全部 stdio sidecar 以状态 65 退出；补齐测试暴露的 `.git` 新建写入拦截、stdio 宿主崩溃用例身份分流，并避免调试 App 被旧 active Launcher 缓存遮蔽。
- [x] 修复 sidecar 启动回归：区分“何时首次启动”与“启动后的运行周期”；需要全局服务能力的存量原生 sidecar 恢复 App 启动时预热，terminal 改为首次终端操作时启动并按 PTY 会话保持，解释器仅首次使用时启动且脚本仍可按清单选择按需或常驻。
- [x] 验证 App 启动阶段应预热的原生 sidecar 已就绪，terminal 和解释器在未使用前不启动，terminal 最后一个 PTY 会话结束后退出且无僵尸进程，解释器常驻脚本首次使用后保持；首条消息不再批量重复启动，App 退出后全部清理。真实沙箱 PTY 命令返回 `terminal-lifecycle-ok`，格式、相关严格静态检查与全工作区编译均通过。
- [ ] 对不兼容的存量插件制品执行升级或明确拒绝，禁止静默退回无沙箱 TCP。
- [ ] 扩展独立 Sandbox CI，按插件类别验证启动、调用、清理、拒绝边界和打包制品。
- [ ] 运行格式、workspace 构建、相关 clippy、测试与三平台 CI，提交并推送独立分支。

完成标准：所有带 sidecar 的插件都具有宿主强制执行的明确策略；启动时机与运行周期互不混用，需要全局服务能力的原生 sidecar 不把启动成本推迟到首条消息，terminal 只在存在 PTY 会话时运行，解释器不在未使用时提前启动；正常能力不下降，插件不能自行扩大文件或网络权限；旧制品不兼容时由宿主明确拒绝；隔离不可用时拒绝执行；三平台原生验证覆盖实际能力和拒绝边界。

## Windows 完整权限隔离（feature/sandbox-windows-isolation）

范围：当前任务。在现有 Job Object 进程生命周期保护之上，补齐 Windows 文件与网络隔离；不以进程清理代替权限隔离。本分支不改动任何插件代码或插件清单。

- [x] 选择并实现 Windows 原生隔离机制，确保 Launcher 可在普通用户环境创建并应用受限执行上下文。
- [x] 将策略允许的工作区、插件数据目录和临时目录映射为最小文件权限，拒绝用户凭据、工作区 `.git` 与其他路径。
- [x] 默认拒绝网络，并为经过审计的联网插件应用对应策略。
- [x] 确保子进程继承文件、网络、资源和生命周期限制，策略应用失败时拒绝启动。
- [x] 增加 Windows x86_64 原生真实用例，覆盖允许写入、越界读写、网络、后台子进程、取消、超时和宿主异常退出。
- [x] 验证完整 Windows 安装包中的 Launcher 能实际应用隔离并完成自检。
- [x] 运行格式、workspace 构建、相关 clippy、测试与 Windows Sandbox CI，提交并推送独立分支。

完成标准：Windows Launcher 在普通用户环境中只给受限目标授予策略允许的文件和网络权限；所有子进程继承限制；宿主退出、取消、停止、超时或资源超限均不残留进程；能力不可用时明确拒绝；原生 CI 与完整安装包验证通过。command 及其他插件的 Windows 标准流适配归入后续 Sidecar 插件沙箱覆盖任务，本分支不修改插件。

## 附件分析插件多模态停用（fix/analyze-attachment-multimodal-registration）

- [x] 多模态主模型时附件分析插件不再注册工具与提示段（配置钩子载荷附加主模型能力，插件自行判断，主模型切换下一轮跟随）。
- [x] `analyze_attachment` 定位严格化：明确提供 `message_id` 时精确匹配，未命中或无图报错；仅省略时取最近带图用户消息。
- [x] 插件会话快照剔除 Notice 系统通知；轮次锚点改以消息 ID 提供（`turn_start_message_id`，快照字段），idx 保留换算兼容旧版插件。

## 〇、桌面输入队列（feature/chat-input-queue）

- [x] useStore 增加每会话输入队列状态与 `enqueueInputMessage` / `removeQueuedInputMessage` / `steerQueuedInputMessage` / `dequeueNextQueuedInput` 四个动作；投递走"写回草稿 + 现有 sendMessage/appendMessage"复用 revision/claim 校验。
- [x] MessageInput 键盘分流：执行中 Enter 入队（先过 slash 命令拦截），Cmd/Ctrl+Enter 立即引导，空闲 Enter 保持发送，Shift+Enter 换行不变。
- [x] 输入框上方渲染队列条目（内容摘要 + 附件数），支持单独引导/发送与移除。
- [x] turn 结束（runStatus 回 idle）且草稿为空时自动投递队首；投递失败回插队列不重试（上下文管理结束后同样触发）。
- [x] 删除会话时同步清理其队列；队列仅内存态，不做持久化。
- [x] 队列条目支持拖拽调整顺序（`moveQueuedInputMessage` 数组重排，自动放行按新顺序投递）。
- [x] 队列条目支持编辑：内容回填输入框草稿并从队列移除（草稿非空先转入队首），改完按现有按键语义重新入队或发送。

## 一、Plugin Harness

入口文档：

- 需求：`docs/plugin-harness/requirements.md`
- 设计：`docs/plugin-harness-design.md`
- 任务：`docs/plugin-harness/tasks/README.md`
- 进度：`docs/plugin-harness/progress.md`

当前分支：`feature/plugin-name-alignment`。

### 已完成

- [x] Slot、Seam、Contribution 核心类型与注册表。
- [x] manifest schema v2 解析与校验，保留 v1 兼容。
- [x] Host Bridge 命名空间、权限校验和 Tauri 命令。
- [x] Shadow/iframe 沙箱、设置页 Slot 和 `extension.tab`。
- [x] 拓展区矩阵、单例/多实例 App、内置 App 收敛。
- [x] 审批与用户交互接缝原型。
- [x] 三方 UI 插件 SDK、模板和开发说明。

### 当前待办

- [x] 将 `request_user` 的工具定义、参数解析、六类请求编排和结果生成迁入交互处理器 TypeScript 代码，删除 Core 与公共运行时中的专用处理器。
- [x] 将交互处理器完善为可安装、可运行的纯 TypeScript 插件，覆盖六种请求类型、2 分钟倒计时、重复提交禁用和闭合状态。
- [x] 提供中立的 TS 工具调用桥接：清单声明工具，宿主转发不透明调用并等待插件结果，不修改 `plugin.wit`。
- [x] 接通 `tool.requested` / `tool.closed` 事件与 `tool.resolve` Bridge；宿主只校验调用归属和唯一闭合，不解释审批结果。
- [x] 修复同一插件多实例订阅相同事件时，一个实例退订影响其他实例的问题，并补充引用计数单元测试。
- [x] 删除运行期审批挑战和授权状态；插件收集用户意见，Agent 决定后续步骤。
- [x] 完成针对最终方案的单元测试、完整构建和代码审查；Desktop 实际交互由用户验证。
- [x] 修复交互处理器在消息流中错位、深色主题不可读及审批文案仍带授权含义的问题。
- [x] 交互处理器显示时覆盖并锁定输入区，提供可向 Agent 回传关闭原因的统一关闭入口。
- [x] 保证交互处理器完整显示，普通内容不滚动，仅在文本、选项或表单超出可用空间时滚动中间内容区。
- [x] 将默认交互处理器从 iframe 迁为动态 Shadow 插件，并作为会话区域独立弹层挂载，不再嵌入输入组件。
- [x] 扩展通用 Shadow 脚本契约，提供插件根节点、初始/动态宿主上下文和卸载回调，同时兼容只使用 `bridge` 的旧脚本。
- [x] 保持第三方 Shadow 与 iframe 声明均可用；插件安装、更新、禁用或卸载时动态挂载和清理对应实例。
- [x] 接通会话输入区 Slot 宿主，允许已授权插件向当前输入草稿添加经过校验的附件。
- [x] 提供可安装的截图输入插件：入口紧邻附件按钮，原生交互选择区域/窗口，截取 PNG 加入当前草稿且不自动发送。
- [x] 验证截图取消、权限拒绝、超限图片和插件卸载后的安全行为。

## 二、Agent Core 文档治理

现行架构入口：

- `docs/agent-loop-refactor/design.md`
- `docs/core-architecture.md`

### 已完成

- [x] 核对当前代码中的 `TiangongCore`、`shared_runtime`、turn task、per-turn 命令通道和引导消息处理。
- [x] 删除常驻 Driver/Agent Inbox 的迁移规划文档。
- [x] 从 PLAN/TODO 中移除已淘汰方案的实施任务和完成标准。
- [x] 重写现行 Agent Core 架构说明。
- [x] 更新仓库开发指引，移除旧 `src/core` planning/execution 架构描述。

### 文档约束

1. 当前代码没有常驻 Agent Driver，也没有 Agent Inbox。
2. 用户消息入口为 `AgentInputKind::Message`：空闲时创建 turn，运行中发送 `Command::InjectUserMessage`。
3. “复用引导消息处理”仅指复用上述真实路由和 `save_user_message_and_restart` 行为。
4. 历史架构只从 Git 历史追溯，不得重新写入当前任务和完成标准。

## 三、终端插件显示修复

- [x] Shadow 桥接并发订阅多个事件时只注册一份全局监听，确保每批终端输出只写入一次。
- [x] sidecar 启动前等待事件订阅完成，并缓存终端附着前的启动输出。
- [x] 移除反复强制滚动和诊断角标，修正终端容器尺寸与滚动区域。
- [x] 完成 Rust 格式检查、前端/终端插件构建及真实浏览器画布验证。
- [x] 提交变更，等待 Desktop 实际交互验证。
- [x] 宿主上下文注入会话工作目录（hostContext.session.workspace），终端插件默认 shell 以当前会话 workspace 为初始目录，与内置终端面板一致；cwd 失效时回退不阻断。
- [x] 终端跟会话走：每个会话独立 PTY（scope_id 关联，工具会话同体系），切换会话和暂时隐藏时恢复原终端；明确关闭终端 App 标签时结束旧终端并清除恢复记录，再打开创建新终端；会话退出即出表。
- [x] 跨重启恢复（对齐内置终端日志方案）：会话输出按 scope 追加落盘到插件数据目录（1 MiB 滚动）；重启后无存活会话时 find 返回磁盘日志尾部（经 vte 行处理器剥离控制序列，防重放触发 xterm 响应污染新 PTY），UI 新建 shell 并把历史回填在上方。
- [x] 验证终端 App 明确关闭后不再恢复旧进程和旧输出。
- [x] 修复插件热更新后终端通知连接失效，以及容器重建时旧恢复任务误报 bridge 已卸载的问题。

## 四、浏览器插件界面与交互对齐

- [x] 移除浏览器插件内部标签栏，使每个浏览器页面直接对应一个 App 拓展区顶部标签，并可与终端标签混排。
- [x] 由 App 顶部标签统一处理浏览器页面的新建、切换、关闭、标题更新和会话恢复。
- [x] 修复浏览器原生页面在切换到终端后仍可能停留在可视区域的问题，恢复终端输入、回显和重新显示后的画面刷新。
- [x] 保留地址导航、加载/失败反馈、后退、前进、刷新、缩放、快捷键、历史记录和批注操作。
- [x] 保持浏览器跟随会话、关闭重开恢复以及 Agent 与用户共用页面的现有插件化行为；`web_fetch.open` 只控制拓展区是否自动展开，静默读取仍建立浏览器 App，用户手动打开时聚焦 Agent 当前页面。
- [x] 完成浏览器插件、终端插件和宿主前端本地检查；未改动的 Rust 浏览器引擎不重复检查，Desktop 功能由用户验收。
- [x] 修复页面已显示但完成事件未被接纳的问题，避免刷新按钮持续旋转并在时限到达后误报失败。

## 四-A、插件实际调用修复

- [x] SDK 提供 App 插件打开接口及是否自动展开拓展区参数；宿主仅允许声明 `extension.tab` 且具有 `app.use` 权限的插件调用。
- [x] 浏览器插件自行消费 `web_fetch.open`，宿主 webview 引擎不再根据工具参数决定是否展开界面。
- [x] 页面进入可读状态后立即结束浏览器等待，不再固定等到 30 秒截止时间。
- [x] `run_command` / `run_shell` 创建或复用对应的长期交互终端，在后台建立 App 标签但不自动展开拓展区；执行后保留提示符，向 Agent 返回真实输出与退出状态，并避免多个插件实例重复执行同一调用。
- [x] MCP 动态函数名正确映射回带连字符等字符的真实 server/tool 名称。
- [x] 完成相关插件打包、Rust/前端检查与本机插件更新。
- [x] 修复工具创建的终端停在“正在连接终端”占位：预热后仍建立输出监听，并立即附着已创建的终端内容。
- [x] 三次审查符号链接真修复：根因是 statSync 跟随链接且 path.resolve 不解析链接（上轮防护未生效）；新增 assertNoSymlinkPath 逐级 lstat 断言（build/validate/run 全接入），resources 根非真实目录显式报错，零构建失败整体清 release；六项攻击场景重放全部拒绝且零泄漏零残留，正常链路回归通过。
- [x] 二次审查四项修复：devkit 路径越界（resolveInside + lstat 逐项复制，build/validate 共用，攻击重放零泄漏）；安装确认时序（先暂存后确认，确认对象为不可变事务副本，拒绝自动清理）；yarn 缺失结构化错误（不再崩溃，含 toolchain 探测）；超时进程树终止（Unix 进程组 / Windows taskkill，等真正退出）。
- [x] npm 可信发布（Trusted Publishing，OIDC）验证通过：workflow 免 token 改造（npm ≥ 11.5.1 升级、裸 publish、provenance 自动）；1.0.0 本地手动发布占位 + 包设置登记 Trusted Publisher 后，tag devkit/v1.0.1 经 CI 成功发布（registry 双版本、latest=1.0.1）；真 npx 公网拉包五命令全链路（init→validate→build→run 真实 tsx 执行→logs）全部通过。
- [x] 功能验证：三模板完整开发旅程通过（ts-npx 含改码后真实 npx 执行、ui-app 改页构建、ts-tool yarn 全链 13s）；install 完整链集成测试（确认桩→暂存→导入→注册表，46 项测试）；CI 发布链路验证（触发/版本校验/打包干跑正常），npm 侧待用户换 Automation token 后重推 tag。
- [x] 五轮评审修复：sendText 收敛为「用户普通 Enter」语义（submitExternalText：草稿保护、运行入队不引导、信任模式透传、发送事务中直接队列项防重复）；mention 聚合锁外调用 + (kind,value) 去重；运行判断对齐既有约定；devkit v1.0.2 发布（含 saveFileDialog 模板，tag 与包内容一致，CI 可信发布 28s 成功）。前端 218 项 / Rust 46 项测试全过。
- [x] plugin-creator v0.1.0 正式发布：PR #445 合并后首发失败（目录命名拼接 tiangong-plugin-plugin-creator 探测不到 + resolve 失败级联），修复 PR #446 合并后重发成功（4m56s 三平台全绿）；OSS catalog 已上架（manifest + UI 制品抽查可访问，checksum 齐全），插件市场可搜索安装。
- [ ] Desktop 实际交互由用户验收：浏览器及时返回并按参数展开、MCP 正常调用、终端在后台创建或复用且执行与定向输入结果正确回传。

## 四-B、取消后台隐藏 App 实例策略

- [x] `app.open` 建立的实例统一进入拓展区顶部标签（可见、可关闭、计入在用绿点）；`showPanel=false` 仅表示不自动展开拓展区面板，工具静默拉起的终端/浏览器不再有用户不可见、无法关闭的隐藏实例（修复“关闭全部终端后绿点常亮”）。
- [x] 宿主无订阅兜底拉起（不带实例编号的 background）与后台会话（Sub Agent/Bot）仍走隐藏执行壳，只保证工具有人接应，不建标签、不亮绿点。
- [x] 矩阵图标左键改为“有则聚焦（含工具建的标签）、无则新建”；多实例 App 右键补“新建实例”入口；关闭某 App 的全部标签即熄灭绿点。
- [x] `terminal_open` 恢复弹出并聚焦拓展区面板（工具语义“向用户展示”），`run_command`/`run_shell` 保持静默建标签不弹面板。
- [x] 前端构建与 212 项测试、终端插件构建、宿主 cargo check 全部通过；Desktop 实际交互由用户验收。

## 五、插件安装性能优化

- [x] 安装、导入或升级单个插件时直接检查目标插件，不扫描和校验其他已安装插件。
- [x] 保持目标插件的版本检查、签名校验、原子替换、失败恢复和热加载行为不变。
- [x] 记录安装锁等待、目标校验、目录切换和总耗时，便于定位剩余慢点。
- [x] 完成插件运行时相关格式检查、测试和构建验证。

## 六、CI 任务范围优化

- [x] 公共插件 SDK 变化只检查实际依赖 SDK 的界面插件，不触发无关 Rust 插件和 sidecar。
- [x] 插件 sidecar 仅在插件自身或公共 sidecar 代码变化时展开 Linux、macOS、Windows 矩阵。
- [x] 无 Bot 文件或 Bot CI 配置变化时不启动 Bot CI。
- [x] 用当前 PR 文件列表模拟任务矩阵，Plugin CI 从约 80 个任务降到约 10 个，且直接变更项无遗漏。
- [x] 完成工作流语法、差异和本地模拟检查。

## 七、官方 Desktop 插件命名统一

- [x] 终端插件 ID 改为 `terminal`，项目目录和工程名改为 `tiangong-plugin-terminal`。
- [x] 审批征询插件 ID 改为 `interaction`，项目目录和工程名改为 `tiangong-plugin-interaction`。
- [x] 嵌入式浏览器插件 ID 改为 `browser`，项目目录和工程名改为 `tiangong-plugin-browser`。
- [x] 同步插件 ID、宿主引用、终端 sidecar、构建配置、锁文件和现有文档，确认旧名称无残留。
- [x] 完成三个插件、宿主前端和 Rust workspace 的相关构建检查。

## 七-A、发布官方 Desktop 三插件到 OSS 目录

当前分支：`feature/publish-official-ui-plugins`。

- [x] xtask 支持无 WASM 的发布形态：`PluginConfig` 的 protocol/wasm 字段改为可选，注册 `interaction`、`browser`、`terminal` 三个插件。
- [x] 纯 UI 插件（interaction/browser）发布目录无 wasm 条目、无 sidecar 与签名清单；terminal 保持 sidecar 官方签名与三平台矩阵。
- [x] 终端 sidecar 以 `[[bin]]` 固定制品名 `tiangong-terminal-sidecar`，对齐其余 sidecar 插件约定；同步本地构建脚本。
- [x] browser/terminal 的 package.json 版本与 plugin.json 对齐（0.1.0）；interaction 已一致（0.3.1）。
- [x] 宿主 `PluginRelease.wasm` 改为可选：下载端按 plugin.json 声明决定是否拉取 wasm，目录校验对无 wasm 条目放行、对"清单与目录不一致"双向报错。
- [x] `publish-plugins.yml` 按插件形态选择任务：resolve 阶段解析 has_wasm/has_ui，跳过无 wasm 的共享 WASM 构建，UI 单文件在 ubuntu 一次预构建后由三平台复用。
- [x] 本地验证：三插件 validate/build/本地部署、目录校验、与远端现网 catalog 的 merge-plugin-catalog 合并，以及既有 18 个插件与 prompt 全量构建回归。
- [x] 合并后依次打 `plugin/interaction/v0.3.1`、`plugin/browser/v0.1.0`、`plugin/terminal/v0.1.0` 标签触发 OSS 发布；修复发布链 skipped 传播问题（build-plugin/prepare-release/publish-plugin 显式判定上游结果），三个插件已上线，线上目录 21 个插件，制品与签名全部回读验证通过。

## 七-B、插件自动安装与自动升级（含卸载记忆）

当前分支：`feature/plugin-auto-maintenance`（已合并 #426）；移除自动安装在 `feature/remove-plugin-auto-install`。

- [x] 核心插件集合 `AUTO_INSTALL_PLUGIN_IDS`（terminal/browser/interaction）；interaction 并入 `DEFAULT_PLUGIN_IDS` 与日常/编程分类。
- [x] 卸载记录 `uninstalled_plugins.json`（原子写、损坏容错）：`uninstall_plugin` 成功写入，`install_staged_plugin` 成功清除，手动重装即解除记忆。
- [x] `list_available` 补充 `installed_enabled`（读取安装目录 `.disabled`）。
- [x] `plan_auto_maintenance` 决策：核心缺失自动安装、启用且有更新自动升级；黑名单与禁用插件跳过。
- [x] 宿主启动后台任务 `start_plugin_auto_maintainer`：拉目录 → 计划 → 串行安装/升级 → 广播 `plugins_changed`；离线与失败仅日志，下次启动重试。
- [x] 首启推荐引导不再重复推荐自动安装的三个核心插件。
- [x] 集成测试 5 项（记录往返、损坏容错、计划决策、registry 卸载/重装集成）；Rust 130 项与前端 212 项测试全绿。
- [x] v0.15.0 发布后存量用户已自动补齐核心插件，移除自动安装：删除 `AUTO_INSTALL_PLUGIN_IDS` 与安装计划，任务收敛为 `start_plugin_auto_updater`（仅自动升级），首启推荐引导恢复推荐全部默认插件。
- [x] 自动升级策略简化：直接按插件目录的可更新状态与插件启用状态触发升级（`update_available && installed_enabled`），删除计划函数与卸载记录机制（已无消费者）；`installed_enabled` 字段保留。

## 八、验证要求

文档改动至少执行：

```bash
rg -n -i 'Agent Inbox|唯一 driver|常驻 Driver|统一通道' PLAN.md TODO.md CLAUDE.md docs
```

代码功能改动按项目范围执行：

```bash
cargo fmt -- --check
cargo check --workspace
cd frontend && yarn build && yarn test
```

Rust 核心或插件运行时变更另行执行相关 crate 的 clippy 和测试。

## 九、清理旧的终端/浏览器宿主 crate

当前分支：`feature/cleanup-legacy-terminal-browser`。

- [x] webview 引擎原语（manager/watcher/handler/page_fetcher/session_registry/types/bridge/js 注入脚本）从 `crates/plugins/tiangong-plugin-browser` 迁入 `src-tauri/src/webview_host/`，删除 Tauri 插件包装、命令层与 Core Plugin 工具注册。
- [x] 删除终端旧链路：宿主 PTY 管理、`terminal_*` 命令、`terminal:set_cwd`/`sync_workspace_cwd`、`terminal:user_command` 注入监听与旧 session store 迁移调用。
- [x] `core_factory` 不再无条件注入终端/浏览器工具，全部由已安装插件提供；`DEFAULT_PLUGIN_IDS` 加入 `terminal`、`browser` 保证首启引导推荐安装。
- [x] 前端删除 native 容器组件（TerminalPanel/TerminalTabContent/BrowserTabContent）、`plugin:terminal|*`/`plugin:browser|*` 命令封装与终端 tab 同步监听；旧 kind 存量 tab 在恢复时丢弃。
- [x] 删除两个旧 crate 目录，清理 workspace 成员/依赖、capabilities 权限、pre-commit 排除目录与锁文件。
- [x] 验证：`cargo check --workspace`、clippy、fmt、`cargo test -p tiangong-app` 与 `-p tiangong-plugin-runtime`、前端 `yarn build` 与 `yarn test` 全部通过；全仓库扫描旧 crate 引用无残留（历史文档除外）。

已知取舍：终端用户命令注入 Agent 对话（native 容器上报链路）随旧 crate 一并移除，新终端插件暂无对应功能；会话删除时终端插件 sidecar 的 PTY 不随宿主清理（插件自管理生命周期）。

## 十、无效插件标记与清理

当前分支：`feature/invalid-plugin-cleanup`。

- [x] 插件扫描发现无效插件（签名无效、沙箱声明越权、清单损坏）时登记到注册表，不再静默忽略。
- [x] 无效插件以 `invalid` 状态出现在插件管理列表，展示被忽略原因（如部署残留目录签名文件不完整）。
- [x] 提供清理入口：无效插件复用单个插件的卸载逻辑（含保留数据选项），不新增专用清理方法。
- [x] 验证：`cargo check`、`cargo test -p tiangong-plugin-runtime`、前端 `yarn build` 通过；模拟签名不完整/清单损坏目录的登记、展示与卸载已临时端到端验证。

## 十一、LLM 确定性错误不再回退非流式重发

当前分支：`fix/stream-deterministic-error-no-fallback`。

- [x] 流式请求失败后，配置错误、认证失败、请求无效（400 类）等确定性错误直接失败，不再回退非流式重发注定失败的请求；网络、超时、限流等瞬态错误保持回退行为。
- [x] 错误映射保留 `LlmError` 类型信息，供回退决策准确分类。
- [x] 验证：`cargo check --workspace`、`cargo clippy`、`cargo test -p tiangong-llm` 全部通过；`tiangong-core` 相关测试断言更新为新行为后单独运行全部通过（并发全量下偶发失败为 main 已有的测试不稳定，与本次改动无关）。

## 十二、发布 v0.15.0 后调整的四个插件（chore/publish-adjusted-plugins）

- [x] 版本提升合并 PR #436（单提交 20a1f16d）：analyze-attachment 0.1.0 → 0.1.1（#435）、index 0.1.0 → 0.1.1（#435）、memory 0.1.2 → 0.1.3（#435）、terminal 0.1.0 → 0.1.1（#429）；同步 plugin.json、terminal package.json、各 crate Cargo.toml 与 Cargo.lock，协议版本不动。
- [x] 打标签 `plugin/analyze-attachment/v0.1.1`、`plugin/index/v0.1.1`、`plugin/memory/v0.1.3`、`plugin/terminal/v0.1.1`（多标签推送未触发 workflow，改用 workflow_dispatch 逐个触发，构建源同为 main HEAD）。
- [x] 四次发布运行全部 success；线上 catalog 回读四个插件版本均已更新，releases/<id>.json 与三平台制品（含校验和与签名清单）抽查可访问。

## 当前完成标准

1. PLAN、TODO、CLAUDE 和 Core 架构文档对现行执行模型描述一致。
2. 仓库入口文档不再把常驻 Driver、Agent Inbox 或旧 planning/execution 分层描述为当前实现。
3. Plugin Harness 后续设计以真实代码路径为依据。
4. 当前分支最终功能通过完整构建、相关测试和交付审查。
5. 浏览器页面与终端共同使用 App 拓展区顶部标签，插件内部没有第二套标签栏；其余界面、可见功能和主要操作路径与原内置浏览器一致。
6. 终端、审批征询和嵌入式浏览器插件统一使用能力名插件 ID，并使用 `tiangong-plugin-*` 项目目录与工程名。

## 十三、plugin creator 一期（RFC 0017 §11 / S5.1）

当前分支：`feature/plugin-creator`（worktree：../tiangong-plugin-creator，基于最新 main）。

- [x] 宿主 `plugin-dev.*` 受限桥接通道：manifest 新增 `resources` 静态资产字段与导入复制；桥接命名空间 `plugin-dev.` + `plugin-dev.use` 权限；服务实现 init/list/validate/build/install/logs/status，写范围锁定 `~/.tiangong/plugins-dev/`，ID 白名单防路径逃逸与劫持（含防自举），构建并发互斥，安装 fail-closed 依赖宿主原生确认。
- [x] plugin-creator 官方插件（纯 TS 工具插件，无 sidecar）：extension.tab「插件创作」页（模板选择、项目列表、构建状态、安装历史）+ plugin_init/build/install/validate/logs 五工具（页面与 Agent 共用同一 plugin-dev 后端）+ 生成规范 prompt。
- [x] 模板与仓库同源随包分发：ui-app（零构建，宿主内建打包，无需 Node）；ts-tool（interaction 同款结构，vendor 内置 SDK 免网络依赖，package 生成内容树清单供哈希锁定消费）。
- [x] src-tauri：安装原生确认对话框注入（tauri-plugin-dialog，非 webview）；plugin-dev.* 桥接经 spawn_blocking 执行防阻塞。
- [x] 验证：cargo check/clippy --workspace 与 47 项单测通过；creator 与 ts-tool 模板真实 yarn install/build/package 全链路通过；两个 release 包经真实导入暂存逻辑（stage_local_plugin）冒烟验证。
- [ ] Desktop 实际交互由用户验收：导入 plugin-creator → 创作页新建项目 → 构建 → 自动签名安装 → Agent 对话调用五工具。

## 十四、审查修复与 ts-sidecar 二期（npx 形态，RFC 0017 S6.1）

> 分支边界拆分（用户决策：creator 分支只关注 creator；npx 执行能力走命令通道）：
> 原 sidecar 路线的二期实现（merge plugin-trust-model、npx sidecar 宿主层、
> 按需一次性执行、ts-sidecar 模板）已废弃，承载分支 `feature/ts-sidecar-npx`
> 已删除；ts-npx 模板（命令通道方案）见下节，最终以 `feature/plugin-creator`
> 四提交交付。

`feature/plugin-creator`（worktree：../tiangong-plugin-creator）仅含：

- [x] ts-npx 模板与 plugin_run 试运行（用户决策：npx 执行能力直接复用命令通道）：CLI 式脚本 + 说明书式插件（prompt 教 Agent 经 run_command 执行），零 sidecar、零帧协议、零宿主执行层、不依赖沙箱分支；零构建打包补 resources 复制；创作页试运行按钮。模板脚本真实 npx 运行验证通过（隔离缓存、stdout 纯 JSON）。
- [x] 外置化（用户方案）：业务工具链迁至 npm 包 @silent-ai/plugin-creator（devkit CLI v1.0.0，plugins/devkit/；@tiangong 组织名在 npm 被占用故定案此名），Agent 经命令通道执行 npx -y @silent-ai/plugin-creator@1.0.0 init/validate/build/run/logs；宿主 plugin_dev.rs 瘦身至 install/list/status（1704→440 行）；模板随 devkit 分发；插件工具面收缩为 plugin_install；创作页看板化。devkit 五命令真实链路与导入暂存验证通过。npm 发布（npm publish --access public）待用户执行。
- [x] 纯 TS 插件 @提及支持：manifest 新增 mention 声明（hint），TsPluginAdapter 覆写 MentionCandidateProvider（原空实现），候选 `@plugin:<id>` 进输入框补全；ts-npx/ts-tool 模板已声明，与说明书 prompt 互补（说明书教 Agent 用，mention 供用户点名）。

- [x] 一期审查三项修复：read_log_tail UTF-8 边界 panic（发布阻断，含中文回归测试）、零构建扁平入口漏复制、install 与 build 共用互斥（独立 fix 提交）。
- [x] merge feature/plugin-trust-model（L3/L4 信任、宽松沙箱 Launcher、未签名保守默认），三文件冲突面零冲突。
- [x] 宿主 npx 运行时形态：manifest `sidecar.runtime: "npx"` + 调用描述校验（精确版本/参数白名单/防路径逃逸）；stdio spawn 解释器参数与缓存隔离（npm_config_cache 指插件数据目录）；宿主规则 runtime=npx ⇒ 沙箱+放行网络+stdio；修复 sidecar_connection 策略解析硬编码（未签名插件现走保守默认）。
- [x] L3 串联：受控延迟导入（import_staged_plugin_deferred）+ plugin-dev.install 一次原生确认（含沙箱与联网警示）→ 安装目录内容哈希锁定 → 预热拉起；正式导入 fail-closed 不变。
- [x] ts-sidecar 模板：sidecar/main.ts node 帧协议完整实现（npx tsx 直跑、无编译链），UI 双路调用示例；creator 枚举/规范/卡片更新。
- [x] 验证：clippy 零警告、全 workspace 测试通过；npx sidecar 真实端到端握手（真实下载 tsx 完成 auth/handshake/业务调用）；模板真实构建链与导入暂存通过；creator 重新打包。
- [x] npx sidecar 改为按需一次性执行（用户决策）：每次调用时宿主临时拉起 npx 进程（恒沙箱+联网+缓存隔离）完成单次往返后退出，无常驻连接、无预热；信任门槛（L1/L3/L4）提取共用，未信任即拒；安装/加载/预热链对 npx 跳过常驻路径。
- [ ] Desktop 实际交互由用户验收：含 sidecar 项目安装弹窗（沙箱/联网警示）、L3 重确认、按需调用时沙箱内 npx 下载与运行、断网拦截（真实沙箱拦截需普通终端或 CI 环境——开发机嵌套 Seatbelt 限制，RFC 0017 §17 已记录）。
- [ ] Windows：沙箱受限令牌未接入前 npx sidecar 不可用（macOS/Linux 先行，与 RFC S6 排期一致）。
