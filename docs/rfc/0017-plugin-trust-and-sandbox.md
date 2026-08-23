# RFC 0017 - 插件信任模型与沙箱执行层

关联需求：`docs/requirements.md`（插件信任与沙箱条目）
关联文档：`docs/plugin-harness-design.md`、`docs/plugin-development.md`、`plugins/sdk/README.md`

状态：草案（2026-08-22）

## 1. 背景与动机

当前插件体系存在三个结构性限制：

1. **原生 sidecar 仅限官方**。带 sidecar 的插件必须携带 `tiangong-official` 发布者的
   minisign 签名（`crates/tiangong-plugin-runtime/src/registry.rs` 在 sidecar 启动前硬校验），
   第三方与用户本人均无法创建带原生能力的插件。
2. **"天工自己创建插件"链路断裂**。产品目标是用户可以让 Agent 按需生成插件供自己使用
   （UI + 逻辑全 TS），但重逻辑必须走 sidecar，而 sidecar 只能是官方签名的 Rust 二进制——
   Agent 生成的插件没有可达的落地路径。
3. **命令与 sidecar 执行无 OS 级约束**。`run_command` / terminal 插件 / 各 sidecar 均为
   全权进程，防破坏仅靠命令白名单与审批，缺乏操作系统层的爆炸半径限制。

**业界教训（dsh / DeepSeek Harness，2026-08 公开研究）**：dsh 采用"一切皆插件"架构，
第三方插件进程内加载、先于防护执行、可改写审批与沙箱配置，被验证存在
"提示注入 → 零审批动态插件 → vm 逃逸 → 宿主 RCE"的完整利用链，且不依赖任何配置失误。
根本原因是**安全约束落在错误的层**（安全组件可插拔、加载顺序在第三方代码之后、
安装链零校验）。本 RFC 的多项设计决策直接来源于此教训。

## 2. 完成标准

1. Agent 可以在本机生成"UI + TS sidecar"插件并导入使用，全程不需要官方私钥；
   用户可经 plugin creator 插件以对话或页面方式自助完成同一条链路。
2. 任意插件代码（命令、TS sidecar、Rust sidecar）默认运行在 OS 级沙箱内，
   写入范围受白名单约束，安全配置不可被插件篡改。
3. 白名单内的破坏性操作可通过会话级变更集回滚。
4. 存量插件（官方签名 Rust sidecar、纯 UI、WASM）行为不变，manifest 向前兼容。
5. `tiangong-core`（Agent 循环）零改动，全部新逻辑位于新 crate 与既有接缝。

## 3. 非目标

1. 不追求跨语言逻辑运行时替代 WASM（逻辑层契约不变；本 RFC 修订
   `plugin-harness-design.md` 第 13 章"非目标"中与 TS sidecar 冲突的表述：
   JS/Node 属 sidecar 通道的可选实现，不进入逻辑层）。
2. 不防御外部副作用（`git push --force`、删除远端资源等）——属审批管线职责，
   沙箱明确不覆盖并写入用户文档。
3. 桌面端不引入虚拟机（本地 microVM 常驻内存与文件桥接代价不可接受；
   云端沙箱作为远期扩展位，见 §8.9）。
4. 不做 OS 级按域名网络白名单（不可靠且平台差异大）；网络控制走宿主代理模式。
5. 不重写 Agent Loop 与工具流水线主干。

## 4. 设计原则

1. **入口与执行分离**：信任模型管"谁能装进来"，沙箱管"装进来之后能干什么"，正交互补。
2. **约束落在执行点**：沙箱约束在 OS 层（进程树继承），不依赖被约束方自觉，
   不做成可插拔组件，不接受插件自带 profile。
3. **默认安全 + 精准升级**（Codex 验证的交互模式）：不预测命令能力，先沙箱内执行，
   失败的那条命令按单条粒度升级，会话内信任，审批只在边界处发生。
4. **对模型透明**：沙箱拒绝以结构化信息回传（哪条约束、请求什么、豁免路径），
   Agent 能自主决定改写法或申请扩权，而非盲目重试。
5. **用户是唯一授权主体**：原生对话框确认、密钥导入、豁免决策均不可由 Agent 或插件代劳。
6. **渐进交付**：每个阶段独立可交付、可停在中间态。

## 5. 信任模型：四通道

| 通道 | 授权动作 | 信任锚点 | 适用场景 |
| --- | --- | --- | --- |
| L1 官方签名 | 无需动作（内置公钥） | minisign 官方私钥 | 官方插件市场 |
| L2 第三方密钥导入 | 用户导入一次发布者公钥 | 第三方平台私钥 | 三方平台分发生态 |
| L3 本地确认 | 每插件确认 + 内容哈希锁定 | 用户本人 | 天工自生成插件、单件导入 |
| L4 放开开关 | 全局一次性高危动作 | 无（用户自担） | 插件开发者本地调试 |

### D1. L1 保持现状
官方 minisign 签名通道不变：`publisher == "tiangong-official"`、内置公钥、
`TIANGONG_PLUGIN_PUBKEY_B64` 环境变量保留（CI/本地验证用途）。
多公钥落地后该环境变量语义改为"追加"，避免与公钥库打架。

### D2. L2 密钥导入
- 公钥库存宿主数据目录（插件不可达），条目：`{publisher_id, pubkey_b64, name,
  fingerprint, imported_at, capability: "full" | "ts-sidecar-only", source}`。
- **发布者身份以宿主侧 key 绑定为准**，release.json 自声明 publisher 仅作展示比对
  （防伪造官方发布者）。
- **插件 id 钉扎**：安装时记录 id 与 key 指纹绑定，升级必须同 key；官方插件 id 列表
  为保留字（防同名劫持顶替官方插件）。
- **能力档位在导入时选定**：完全信任（含原生 Rust sidecar）/ 受限（仅 TS sidecar，
  与 L3 同界）。导入界面显示 key 指纹，官方 key 内置不可删。
- 移除第三方 key 时该发布者名下插件一并禁用（不自动卸载）。

### D3. L3 本地确认（TOFU + 内容哈希锁定）
- 未签名插件声明 TS sidecar → 安装阻断 → **宿主原生对话框**（非 webview，防 Agent 借
  computer-use 代点）展示：来源路径、能力清单逐项、内容指纹、破坏性警示。
- 授权记录 `(plugin_id, 内容哈希, 授权能力, 时间, 来源)` 存宿主信任库。
- **内容哈希覆盖**：plugin.json + sidecar 入口 + package.json + lockfile +
  sidecar 源码目录树清单（文件路径 + sha256 逐条）。内容变更即授权失效，重新确认
  并标注"内容已变更"（堵"先骗授权再改恶意代码"）。
- 授权不可随插件包传递（记录只在本机信任库）。
- 设置页提供 L2/L3 信任清单与一键撤销。

### D4. L4 放开开关
- 语义边界：仅跳过签名门槛（未签名插件可启动 sidecar、声明敏感权限）；
  manifest 结构、协议、路径安全校验**保留**。
- 入口：设置 → 开发者，警示文案 + 输入确认词 + 记录开启时间；开启期间插件管理页
  常驻横幅。
- 开关期间安装的插件打 `unsafe: true` 审计标记，插件列表显眼展示。
- 子选项（防注入）：仅放开手动导入，Agent 自动安装仍需确认。
- 该设置不暴露任何桥接写权限，插件与 Agent 均不可触达。

### D5. 通道 × 能力矩阵

| 能力 | L1 | L2 受限 | L2 完全 | L3 | L4 |
| --- | --- | --- | --- | --- | --- |
| 纯 UI / WASM / Shadow | ✅ | ✅ | ✅ | ✅ | ✅ |
| TS sidecar（node） | ✅ | ✅ | ✅ | ✅ | ✅ |
| TS sidecar（python） | ❌ | ❌ | ✅ | ❌ | ✅ |
| 原生 Rust sidecar | ✅ | ❌ | ✅ | ❌ | ✅ |
| native 容器 / 敏感存储 | ✅ | ❌ | ❌ | ❌ | ✅ |

理由：L3 的强制执行依赖 launcher + Node `--permission`（Node 24+ 稳定，含网络安全修复），
Python 无对等物；L2 受限档与 L3 同界。

## 6. 运行时类 sidecar（node / npx / python / pipx）

### D6. 运行时与入口
manifest `sidecar.runtime` 取 `node | npx | python | pipx`；`entry` 为脚本相对路径
（脚本形态）或精确版本的包名（包名形态，禁止范围符号与 latest）。
**宿主不直接调用 npx/pipx 执行**（隐式下载即执行、缓存共享、无校验插点），
包名入口一律展开为受控流程：下载到插件隔离目录 → 校验 → 以 node / python 直启入口。

### D7. Launcher（`@tiangong/sidecar-runtime`）
宿主实际 spawn 的是官方 launcher：`node launcher.mjs --entry ... --policy ...`。
launcher 把 manifest 能力声明翻译为 Node 启动参数
（`--permission --allow-fs-read/--allow-fs-write/--allow-net/--allow-addons ...`），
未声明能力进程内直接拒绝；同时封装帧协议（认证、握手、dispatch、通知）。
Python 侧无权限模型对等物，v1 不进 L3/L2 受限档（见矩阵）。

### D8. 受控安装硬规则
- npm：`npm ci --ignore-scripts`（禁 postinstall 等生命周期脚本投毒入口）。
- Python：`--only-binary=:all:` + `--require-hashes`（wheel 安装不执行 setup.py，
  逐包哈希核对）。
- 安装仅落 `<插件目录>/runtime/deps/`，不碰全局；每次启动校验缓存哈希（防投毒）；
  升级依赖 = 改清单 = 重新走签名/确认。
- npx/pipx 语义仅作为 manifest 入口的书写形式，执行一律走受控安装。

### D9. 制品签名锚定
release.json 的 sidecar 条目扩展：
- 脚本形态：`content_manifest`（目录树"路径+sha256"清单文件）参与签名，运行时按清单
  重扫目录，不一致拒绝启动。
- 包名形态：`external` 列表（registry + name + 精确 version + integrity：npm sha512 /
  pypi sha256）参与签名，下载后逐包校验。
- 确认窗与插件详情页展示全部外部依赖包名清单（签名防篡改，不防恶意作者——展示层
  留给人判断，生成规范要求 Agent 为每个外部依赖写明用途）。

## 7. 沙箱执行层

### 7.1 威胁模型

目的：**防止 Agent 或插件对计算机造成破坏性行为**。破坏分层与对策：

| 破坏层 | 例子 | 对策 | 负责层 |
| --- | --- | --- | --- |
| 白名单外文件破坏 | 删系统文件、覆盖其它项目 | 写白名单 + 防篡改段 | 执行层 |
| 安全配置篡改 | 改信任库、写启动项后门 | 固定防篡改段 | 执行层 |
| 白名单内破坏 | `rm -rf 工作区`、改坏配置 | turn 边界快照回滚 | 恢复层 |
| 外部副作用 | `git push --force`、删远端资源 | 命令审批（沙箱明确不管） | 审批管线 |

### D10. 统一基础设施：一个沙箱、两个策略来源、三个挂载点

```
crates/tiangong-sandbox（唯一实现：policy IR + 三平台编译器 + runner + 快照服务）
        ↑                     ↑                      ↑
  策略来源 A：命令执行      策略来源 B：sidecar
  （会话模式 + 升级状态）   （manifest capabilities + 豁免记录）
        │                     │
  挂载点：run_command / terminal PTY / sidecar spawn
```

底层原语、固定防篡改段、journal 共用；两个执行位只是策略来源不同。
所有插件代码（含官方 Rust sidecar）默认处于防篡改段保护下——官方供应链被污染时
爆炸半径受限（信任 + 限制的组合，替代"要么全信要么不装"）。

### D11. 命令执行位（Codex 骨架）
- 三档模式：`read-only` / `workspace-write`（**默认**：读全盘放行、写限工作区+临时目录、
  网络默认禁）/ `full-access`（显式）。
- **命令预分类**（assess_command_safety）：静态评估已知安全 / 已知危险 / 未知，
  未知默认沙箱内执行。
- **失败升级**：EACCES / 网络拒绝 → 结构化错误
  （`code: sandbox_denied` + 约束条目 + 请求对象 + 豁免路径）→ Agent 改写法重试或调
  `request_capability` 申请升级 → interaction 审批（kind: approval）→
  单次沙箱外重跑 / 本会话信任该命令 / 拒绝。全程审计日志。
- `request_capability` 工具：Agent 只能发起，决策必经用户。

### D12. sidecar 位：继承式沙箱
- **sidecar 整体进沙箱，约束随进程树自动继承**：Linux 经 bwrap 起 sidecar，
  macOS 经 `sandbox-exec`，Windows 以受限令牌启动。sidecar 被 fully 攻破时其一切
  exec 仍在笼内（无 wrapper 可绕——这是与逐命令包装方案的本质安全差异）。
- **全权命令通道分离**：sidecar 在沙箱内注定跑不了全权命令（特性）。升级命令由
  **宿主直接 spawn**（策略来源 A 的 runner），审批在前，不经 sidecar。
  terminal 侧的全权需求由宿主另起非沙箱会话。
- **invoke 层动态检查**：OS 沙箱 profile 在 spawn 时固定，fs 等动态能力 sidecar 的
  路径在宿主 `sidecar.invoke` 转发时解析，白名单外先审批再转发；OS 层做粗粒度兜底。
  动态性在 invoke 层，防逃逸在 OS 层。
- 豁免 = 更新 profile + 重启 sidecar（协议已有换代重启机制；terminal 等重启代价高的
  豁免需向用户明示）。

### D13. 平台实现

| 平台 | 主方案 | 回退 | 关键细节 |
| --- | --- | --- | --- |
| macOS | Seatbelt 动态 profile | 无需（全版本可用） | 写根 canonicalize（`/var` vs `/private/var`）；deny network 默认，升级时全开 |
| Linux | bubblewrap（`--ro-bind / /` 全盘只读 + 可写根分层 bind + `--unshare-user/pid`，受限时 `--unshare-net`） | Landlock + seccomp（WSL1、禁 userns 发行版） | 受保护子路径 ro-bind 重只读化；`PR_SET_PDEATHSIG` 父死子灭；PTY 经 devtmpfs |
| Windows | 受限令牌（WRITE_RESTRICTED + 独立受限用户，Codex 同款） | journal-only 弱档（诚实标注） | 读不设限、网络仅代理引导；**ConPTY × 受限令牌可用性为 S6 前置实验项** |

Linux 主选 bwrap 而非 Landlock 的依据：Landlock 无法做可写根内子路径重只读化、
无网络命名空间隔离（Codex 已从 Landlock 迁至 bwrap 为主、Landlock 为遗留回退）。

spawn 时 `env_clear()` 重建环境，防敏感变量泄漏。

### D14. 固定防篡改段（三平台 profile 无条件注入，不可被声明覆盖）
1. 天工宿主目录、信任库、公钥库、设置：只读。
2. 插件自身 plugin.json、签名文件、能力授权记录：只读（不许改出生证明）。
3. 可写根内 `.git` 只读——Agent 能改工作区但改不了 git 历史；`.tiangong` 同。
4. `~/.ssh`、浏览器凭据库、密钥链服务：默认禁读（可显式申请，确认窗最高警示级）。

### D15. 恢复层：turn 边界快照
- 命令的文件写发生在沙箱内子进程，宿主在文件系统层不可截获——恢复层形态为
  **turn 边界快照**（非写前留底）：turn 结束对工作区做快速快照，macOS 用 clonefile、
  Linux 用 reflink/硬链接农场、Windows 硬链接农场（Claude Code checkpoints 验证路线）。
- 提供变更集查看（diff）、按会话节点回滚、单文件恢复。
- 保险丝：快照区容量上限 + 覆盖速率异常检测（暂停并告警，防勒索式填充）。
- 对 core 的触碰仅"turn 结束发事件"一处挂钩。

### D16. stdio 传输（关键路径）
- sidecar 协议增加 stdio 传输：spawn 继承管道，JSON Lines 帧定义不变（protocol.rs 零改动），
  sidecar 侧 IpcServer 与宿主侧连接管理各加一个传输实现。
- 沙箱内**零网络放行**（继承 fd 不受沙箱影响），消灭本机端口探测面；
  sidecar 回调宿主 HTTP（scheduler 等）改经 stdio 由宿主转发。
- 迁移：新 runtime（node/python 系）默认 stdio；存量 Rust sidecar 迁沙箱时一并切 stdio，
  迁移前维持 TCP 不受影响。

### D17. 云端高危档（远期扩展位）
自托管 `e2b-dev/infra`（Apache-2.0，Firecracker microVM + 快照池 + envd）或接 E2B 云服务，
完全不可信脚本丢云端即焚沙箱，本地零负担。桌面沙箱不覆盖的极高危场景由此承接。

## 8. shell 后端抽象

### D18. 探测链与方言声明
- Windows 探测链：WSL bash → Git Bash → PowerShell（前两者给模型真 bash）；
  macOS/Linux 原生。
- 工具描述声明当前后端身份与能力边界（"Windows bash 模拟层：无 systemd/inotify、
  不区分大小写、路径规则特殊"），模型知情主动绕开崩塌区。
- 失败信息标注原因与建议（Cursor 验证：决定 Agent 自主恢复能力）。
- 用户既有 `.bat`/`.ps1` 资产经 `cmd //c` / `powershell -File` 互操作。
- Git Bash 为 MSYS2 POSIX 模拟层：bash 语法全覆盖、日常命令九成以上可用；
  崩塌区（systemd、ELF、inotify、大小写敏感）由 WSL 兜底。

## 9. pipeline（hooks）挂载机制

### D19. 事件点与段接口
- 对齐 Claude Code hooks 模式：`pre_tool_use`（安全门：放行/阻断/升级审批）、
  `post_tool_use`（质量门：可改写结果）、`user_prompt_submit`、`session_start/end`、
  `pre_compact`、`notification`。
- 首批实战段：命令预分类、升级审批、变更集登记。
- 违规事件作为宿主事件广播，企业治理插件可订阅观察。

### D20. hook 权限分级（高权能力的信任约束）
hook 可见所有工具调用参数（等于可见全部会话数据），注册权进信任分级：
v1 仅 L1 官方可注册阻断型 pre 钩子；L2/L3 至多只读观察型 post 钩子；L4 解除限制。
审批交互类 slot（`session.interaction`）仅允许官方签名插件贡献（dsh
"approval → never"教训的天工对策）。

## 10. 与现有系统的关系（core 改动面）

| 位置 | 改动 | 量级 |
| --- | --- | --- |
| `tiangong-core`（react 循环） | 零改动（沙箱在执行层之下，对循环透明） | 无 |
| `plugin-runtime/sidecar.rs` `spawn()` | 插入策略编译 + 沙箱包装 | 小 |
| `sidecar.rs` 连接管理 | 抽象传输 trait，TCP/stdio 双轨 | **中（最大重构点）** |
| `protocol.rs` 帧协议 | 零改动 | 无 |
| `tiangong-plugin-sidecar` | 加 stdio server 实现 | 小 |
| manifest / 信任模型 | 加字段，缺省 = 现行为，向前兼容 | 小 |
| 升级审批 | 复用 interaction `request_user` 全套 | 小 |
| 工具流水线 | 加预分类、变更集登记两个段 | 小 |
| `crates/tiangong-sandbox` | 全部新代码（IR/编译器/runner/快照） | 大（纯新增） |
| toolkit / terminal / command | 接 runner | 中 |

sidecar 沙箱化迁移状态（2026-08-23 实施）：

| 插件 | 状态 | 说明 |
| --- | --- | --- |
| index、skill、coding、generate-image、generate-video、speech-to-text、text-to-speech、analyze-attachment | ✅ stdio + sandbox | 无网络依赖，只写数据目录；已逐一 stdio 握手验证 |
| fetch、generate-image-openai、mcp、scheduler | ✅ stdio + sandbox + network | 网络型（manifest `sandbox_network`），文件写白名单不变 |
| command | ✅ stdio | 命令级沙箱载体，自身再套会与内层沙箱冲突 |
| memory | ⏸ 回退 tcp | IPC 的 recall 流式分支依赖 TCP 连接对象，stdio 适配待 recall 流改造 |
| terminal | ⏸ 保持 tcp | PTY 交互载体，已有独立沙箱包装开关（TIANGONG_TERMINAL_SANDBOX） |
| computer-use、screenshot-input | ⏸ 保持 tcp | 系统平台 API（UIA/AX/截图），沙箱会断能力 |
| fs | ⏸ 保持 tcp | 动态路径访问，等待 invoke 层动态检查（D12）配套 |

验证工具：`cargo run -p tiangong-plugin-runtime --example stdio_handshake -- <二进制> <插件id>`
真实 spawn 完成身份握手；TS 系新 sidecar 生而沙箱化。

## 11. plugin creator：自建插件辅助插件

底层设施（L3 通道、TS sidecar、沙箱）的产品化载体：用户描述想法，Agent 在该插件
辅助下完成"生成 → 构建 → 导入确认 → 迭代 → 诊断"的完整旅程。它是本 RFC 全部机制
的第一个一级消费者。

### D21. 形态与工具面
- **自身是官方签名（L1）的纯 TS 工具插件**：贡献 `extension.tab`（"插件创作"页面，
  模板选择、项目列表、构建状态、导入历史）+ `tool.provide` 工具集，经
  `createToolProvider` 提供（interaction 同款机制），**没有自己的 sidecar**。
- 工具集（Agent 与用户页面共用同一后端）：
  | 工具 | 职责 |
  | --- | --- |
  | `plugin_init` | 按形态模板生成骨架到开发目录（替换插件 ID/名称） |
  | `plugin_build` | vite 构建 + `scripts/package.mjs` 打包 dist + 生成内容树清单 |
  | `plugin_install` | 触发宿主导入 → L3 原生确认弹窗 → 安装启用 |
  | `plugin_logs` | 读取目标插件运行日志（`logs/sidecar.log` 等）辅助诊断 |
  | `plugin_validate` | 清单/结构校验（plugin.json schema、entry 存在、能力声明合法） |
- **生成规范内嵌工具描述**：能力最小化、每个外部依赖写明用途、node ≥ 24、
  交互类 slot 禁用（官方专属）——保证 L3 确认窗里的信息质量（§5 D3、§6 D9 的落地面）。

### D22. 模板库（与仓库 `plugins/templates/` 同源）
| 模板 | 形态 | 参考工程 | 就绪依赖 |
| --- | --- | --- | --- |
| ui-app | 纯 UI 插件（无工具） | 现有 `templates/ui-app` | 无（已存在） |
| ts-tool | Desktop TS 工具插件（UI + 工具提供器） | `tiangong-plugin-interaction` | 无 |
| ts-sidecar | UI + node sidecar（launcher + 帧协议骨架） | 新建 | `@tiangong/sidecar-runtime`（D7） |

### D23. 权限与自举约束
- creator 自身必须官方签名：它持有导入引导、受限写等高权工具，**不可由 L3 自建**
  （防自举：creator 生成的插件走 L3，creator 本身永远是官方的）。
- **写权限经专用受限桥接**（如 `plugin-dev.*` 命名空间），路径白名单锁定
  `~/.tiangong/plugins-dev/<id>/`（骨架、构建产物）与只读日志目录；
  **不可触达**信任库、公钥库、宿主设置（D14 防篡改段对其同样生效）。
- 迭代语义复用 L3：改代码 → 内容哈希变化 → 重新确认（教育用户认识"内容已变更"提示）。
- 开发目录即沙箱靶场：creator 生成的 sidecar 产物在导入时自动进入继承式沙箱（D12），
  creator 自身不管沙箱。

### 用户旅程
1. 用户对 Agent 说"帮我做一个 XX 插件"；
2. Agent 调 `plugin_init` 选形态（或对话澄清后选）→ 骨架生成；
3. Agent 按需求填充代码（普通编码工作流，文件都在开发目录）；
4. `plugin_validate` → `plugin_build`；
5. `plugin_install` → 宿主 L3 原生确认弹窗（能力清单 + 指纹 + 破坏性警示）→ 安装；
6. 迭代：修改 → 重新 build/install → "内容已变更"再次确认；
7. 故障：`plugin_logs` 读取运行日志定位问题。

### 交付分期
- **一期（随 S5）**：`plugin_init/build/install/validate/logs` + ui-app、ts-tool 两模板
  ——用户可自助生成纯 UI 与纯 TS 工具插件。
- **二期（S6 后）**：ts-sidecar 模板（依赖 launcher 与 stdio 就绪）——生成含 node
  sidecar 的完整插件，"天工自建插件"全链路闭合。

## 12. 交付计划

| 阶段 | 内容 | 独立价值 |
| --- | --- | --- |
| S1 | journal/快照恢复层（平台无关、零特权） | 一切可回滚 |
| S2 | stdio 传输改造 | 去监听端口、沙箱前置 |
| S3 | 沙箱基础设施 + 命令位接入（bwrap + Seatbelt，workspace-write + 防篡改段 + 结构化拒绝） | 命令爆炸半径受限 |
| S4 | 升级审批闭环（预分类 + interaction + 会话信任 + request_capability） | 默认安全+精准升级 |
| S5 | sidecar 沙箱化（TS 系直接进；存量四批渐进）+ 信任通道 L2/L3/L4 | 自建插件全链路 |
| S5.1 | plugin creator 一期：init/build/install/validate/logs + ui-app、ts-tool 模板 | 用户自助生成纯 UI / 纯 TS 工具插件 |
| S6 | Windows 受限令牌（前置实验：ConPTY × 受限令牌） | 三平台对齐 |
| S6.1 | plugin creator 二期：ts-sidecar 模板（依赖 launcher 与 stdio） | 生成含 sidecar 插件，全链路闭合 |
| S7 | 云端高危档（e2b 自托管/服务集成，远期） | 极高危即焚 |

工程量参照：Cursor 双平台沙箱全程约三个月；本 RFC 沙箱部分量级相当，
信任模型部分以宿主逻辑为主。

## 13. 风险与已知缺口（第一天透明）

1. **外部副作用不可回滚**：网络破坏只能靠审批，用户文档明示。
2. **确认疲劳**：L3 重复确认可能形同虚设；缓解：改内容必重弹、无"始终信任"快捷项，
   属可接受残余。
3. **Node 权限模型非 OS 隔离**：依赖 Node ≥ 24（含 CVE-2026-21636 修复），
   launcher 探测版本门槛，不满足拒绝启动。
4. **Windows 缺口**：受限令牌只管写；网络仅代理引导非强制；ConPTY 兼容性待实验。
5. **Landlock 回退档**是能力子集（无子路径重只读化、无网络隔离）。
6. **预分类静态分析会误判**：升级审批兜底。
7. **签名不防恶意作者**：合法签名的插件可引用恶意包；靠确认窗展示依赖清单 +
   生成规范要求用途说明。
8. **turn 快照非实时**：同 turn 内的破坏回滚到边界点，中间态丢失属预期。

## 14. 开放问题

1. L2 导入密钥的默认推荐档位：受限（建议）还是完全信任？
2. L4 "Agent 自动安装仍需确认"子选项是否进 v1（建议进，成本低）。
3. python sidecar 是否随 S5 一期交付（建议 v2，先让 node 全链路跑通）。
4. ConPTY × 受限令牌实验结论可能影响 S6 排期。
5. 云端高危档选型（自托管 e2b-infra vs E2B 服务）在 S7 前决策。
6. plugin creator 受限写桥接的命名空间与范围（`plugin-dev.*` 独立命名空间 vs
   扩展既有 fs 桥接）、开发目录固定为 `~/.tiangong/plugins-dev/` 还是允许项目内路径。

## 15. 会话外验证项（无法在 macOS 开发环境完成）

1. **S6 Windows 受限令牌运行时行为**：CreateRestrictedToken(LUA_TOKEN) +
   CreateProcessAsUserW 的核心原语已实现（`sandbox/windows.rs`），但
   输出捕获管道桥接（PROC_THREAD_ATTRIBUTE_HANDLE_LIST 句柄继承）未实现，
   wrap 在 Windows 仍降级直跑；需真实 Windows 环境开发验证后接入主流程。
2. **Windows 交叉编译类型检查**：本机交叉编译被 aws-lc-sys（需 C 工具链 +
   MSVC SDK）阻塞，windows-sys 相关代码的类型验证依赖 CI Windows 任务。
3. **沙箱包装的真实拦截验证**：开发环境处于嵌套 Seatbelt 沙箱
   （可用性探测自动降级），`sandbox::tests` 中的真实拦截测试需在普通终端
   或 CI macOS/Linux 环境执行。

## 16. 业界对标摘要

| 产品 | 沙箱方案 | 本 RFC 借鉴点 |
| --- | --- | --- |
| Claude Code | Seatbelt + bubblewrap/seccomp + 宿主代理网络白名单，开源 sandbox-runtime；权限弹窗减少 84% | 代理网络模式、审批减负交互、checkpoints 快照粒度 |
| Codex | Seatbelt 动态 profile / bwrap（Landlock 已降回退）/ Windows 受限令牌；失败升级 + 会话信任 | 命令位整体骨架、.git 只读、PDEATHSIG、env 重建、平台选型 |
| Cursor | 评估四方案后选 Seatbelt（App Sandbox/容器/VM 否决）；工具结果标注约束信息；约三个月上线 | 对模型透明、防自我篡改（禁写 .vscode 类比禁写 .tiangong）、工程量参照 |
| dsh | OS 级沙箱但入口零校验、安全组件可插拔、加载先于防护 → 4 个实证漏洞含免配置 RCE 链 | 反面教材：约束落层、固化安全组件、入口校验的必要性 |
| E2B | Firecracker microVM + 快照池 + envd，云端即焚 | 云端高危档形态 |
