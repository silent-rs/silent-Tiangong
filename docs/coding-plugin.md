# Coding 插件需求与设计文档

> 插件目录：`plugins/tiangong-plugin-coding/`；版本基线：0.2.0（schema v2）。

## 一、定位

为开发类任务提供工作流约束与可复核证据：项目上下文发现、开发前检查、长任务进度记录、交付前审查。四件套工具由 WASM 声明、native sidecar 执行（git 调用与文件发现），0.2.0 起新增输入框分支切换 UI（仅用户操作，不向模型暴露写工具）。

## 二、工具契约

| 工具 | 语义 |
| --- | --- |
| `coding_project_context` | 发现项目类型、规则/工作流文件、版本控制状态、推荐检查命令；按任务恢复进度（精确匹配优先，无匹配回退最近一条并标注 `latest_checkpoint_task_mismatch`） |
| `coding_preflight` | 修改前核对：任务说明、分支约定（主分支直接工作/分支名前缀）、未提交文件清单（截断 50 条）、规则文件摘要、完成标准（有匹配进度时沿用记录） |
| `coding_checkpoint` | 记录完成标准/进展/改动/验证到插件私有目录；同工作区按任务分槽位（上限 20 条），原子写（tmp+rename），损坏时告警不静默丢失 |
| `coding_review` | 交付前核对：改动范围（上游优先的基线选择）、验证结果（要求 evidence 执行痕迹）、空白错误、diff 规模、风险文件（锁文件/CI/部署/疑似密钥） |

### 关键语义（0.2.0 变更）

- **基线选择顺序**：显式指定 → `@{upstream}` → `origin/HEAD` → `main/develop/master`。feature 分支的上游是任务对照基线，优先于字面量主分支，避免把已合入上游的改动误算成本次改动。
- **验证凭据**：`VerificationResult.evidence { command, exit_code, output_tail }`；声明 `passed: true` 但无 evidence 或 `exit_code != 0` 的条目计入 `unverified_claims`，不计入 `verification_complete`。
  - **边界（如实声明）**：evidence 字段仍由模型自填，插件只做形态校验（命令行非空、退出码为 0）——这提高的是伪造成本与复核可见性，**不是真正的执行核验**。完整闭环需要宿主回填实际执行记录（与 terminal/command 插件打通）或 sidecar 自行执行 `recommended_checks` 并比对，属后续方向，当前版本勿据此假定强保证。
- **路径匹配**：`allowed_paths` 与改动路径双侧剥尾斜杠后做目录前缀匹配（`"src/"` 与 `"src"` 等价）。
- **推荐检查来源优先级**：规则文件（`CLAUDE.md`/`AGENTS.md` 等中"命令程序开头+检查关键词"的行，`&&` 链拆分）→ Makefile/Justfile 目标 → 清单推断（package.json scripts 等）。

## 三、分支切换（用户 UI 专用）

- 挂载点：`session.input-status`（输入区状态行、思考强度左侧；宿主通用插槽，业务 UI 全在插件）。
- 入口：`plugins/tiangong-plugin-coding/app/index.html`（手写自包含单文件，shadow 沙箱，零构建链）。
- 通信：UI → `bridge.call('plugin.branches' | 'plugin.switchBranch')` → WASM `handle_view_message` → sidecar。工作区路径由 UI 从 `hostContext().session.workspace` 取出放进 payload（UI 实例不经 `set_workspace`）。
- 行为：本地分支直接 `git switch`；远端分支本地无对应时 `git switch --track`（自动建跟踪分支）；失败透传 git stderr。仅切换已有分支，不支持新建（V1 范围）。
- **sidecar 生命周期 `on_demand`**：常驻进程的沙箱可写域不随会话切换，切分支需要写工作区权限；on_demand 每请求随会话 cwd 建进程，代价是每次交互冷启动（UI 已适配 loading 态）。

## 四、工程约束

- git 调用：单命令 10s 上限 + 单请求 20s 全局预算（deadline 传递）；只读命令统一 `--no-optional-locks`；stdout/stderr 双捕获（switch 错误透传的前提）；超时读线程 join 回收。
- 文件发现：git 仓库内 `git ls-files --cached --others --exclude-standard`（尊重 .gitignore、覆盖子目录），非 git 回退目录扫描（深度 2）。
- 错误码：参数反序列化失败、工作区/分支不存在映射 `BadRequest`；其余 `ServiceError`。
- manifest：schema v2、`name: "Coding"`（宿主 PluginStatus 展示名优先级：manifest.name → wasm descriptor → id）、`permissions: [sidecar.invoke, bridge.call]`。
- 版本四处对齐：plugin.json = protocol = sidecar = wasm（xtask 只校验前三处，wasm 需手动同步）。
