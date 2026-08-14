# 任务 15：最小 Loop 与工具义务

> 需求：[../requirements.md](../requirements.md) §3.1/3.4 ｜设计：[../design.md](../design.md) §5~6
>
> 状态：**已完成**｜对应方案阶段 3、4、5。

## 目标

1. Loop 收敛为"模型 → 工具 → 模型"：模型无工具调用的响应只形成**候选完成**，经确定性门控后才允许提交（ALR-002/003）。
2. 建立结构化 `TaskContract`：只记录程序可核验的工具义务与证据，不调用第二个模型判断完成度（ALR-006）。
3. 存在未满足义务时使用 Provider 原生工具约束（`tool_choice`），漏发工具时有界修复（上限 2 次），耗尽后明确失败（ALR-007/008/009）。
4. 删除 Summary/ForceFinal/continuation 全部旧完成控制（ALR-305）。

## 实施结果

### 新增 `react/contract.rs`（255 行）

- `TaskContract`：义务集合 + 协议修复计数；`from_session_anchor` 扫描锚点用户消息的 `AssetReference` 块生成第一批义务（入口显式声明，最高置信度来源）。
- `gate_text` / `gate_placeholder`：候选完成门控——空文本与合成占位符是无效输出；未满足义务时模型文字不构成证据。全部同步、确定性。
- `record_tool_result`：只有成功的工具结果满足义务；失败、拒绝、取消保持 Pending（ALR-307）。
- `MAX_TOOL_PROTOCOL_REPAIRS = 2`：独立小预算，只处理协议违约，不参与一般完成度判断。
- 单元测试 4 项（无义务直接完成、有界修复、成功才满足、空文本无效）。

### 主循环收敛（execute.rs：4515 → 3728 行）

```text
NeedModel（有未满足义务 → tool_choice=Any）
→ 模型请求
→ 有工具调用：执行 → 更新 TaskContract → 继续
→ 无工具调用：候选完成门控
   ├─ 无义务且非空：消息标记最终回复 → PendingFinish(Success)
   ├─ 无效输出/有义务（预算内）：注入 tool_protocol_repair 指令 → NeedModel
   └─ 预算耗尽：PendingFinish(Failed，明确说明未能确认完成)
```

- `react_rounds_in_phase` 达到 `max_tool_rounds`（30）时**明确失败**，不再强制模型生成最终回复（ALR-305 语义变更：安全终止而非伪造完成）。
- steer 注入新意图时按新锚点重建契约（`save_user_message_and_restart`）。

### 删除对象

| 类别 | 内容 |
| --- | --- |
| 阶段 | `StartCheckingCompletion`、`CheckingCompletion`、`StartForceFinal`、`ForceFinalPhase` |
| 请求用途 | `LlmPurpose::Summary`、`LlmPurpose::ForceFinal` 及 `complete_llm_request` 对应分支 |
| 模块 | `summary.rs`（425 行：SummaryDecision、ForceFinal 请求构造、解析与提交）、`completion_policy.rs`（183 行：启发式触发策略与实验策略） |
| 预算 | `continuation_count`、`executed_tool_in_phase`、`max_outer_iterations`、`MAX_OUTER_ITERATIONS` 常量 |
| 启发式 | `looks_like_final_answer`（问号结尾判定）及其测试 |
| 续接 | `CompressionContinuation::Summary` 分支 |
| 其他 | `rebuild_system_prompt`（仅 Summary 前使用）、`persist_partial_summary`、中断路径的 Summary/ForceFinal 特判 |

### 保留的兼容项

- `StreamTextKind::Summary` 变体保留（前端 `SummaryText` 事件契约）：最终回复现在以 ReactText 流出并经消息 upsert（phase=Summary）提交，任务 18 前端联调后统一处理。
- `MessagePhase::Summary`（最终回复的消息标记）语义不变。

## 测试

- 任务 13 的 4 项工具义务失败用例移除 `#[ignore]` 后全部转绿：
  - 附件义务纯文本被拒绝；
  - 修复请求后模型返回 tool call → 实际执行 read_file → 成功完成；
  - 连续漏发 → 预算耗尽 → 明确 `Failed`；
  - 工具失败 + 模型声称完成 → 不发布成功。
- 删除 10 项旧策略测试（任务 13 分类）：Summary 触发/NeedMoreWork/工具调用计数/有界迭代/取消迁移、ForceFinal 三项、双失败、outer limit。
- 改写：`runs_tool_then_completes_via_summary` → `runs_tool_then_completes`（工具 + 文本直接完成）；`accumulated_usage_is_aggregated_across_requests` 按两轮请求断言（48 = 18+30）。
- 8 项契约用例全部处于启用状态并通过；`cargo test -- --ignored` 无遗留挂起用例。

## 验证记录

- `cargo fmt -- --check`：通过。
- `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`：通过。
- `cargo check --workspace`：通过（全部下游）。
- `cargo test -p tiangong-core`：93 通过、0 失败、0 ignored。
- `cargo test -p tiangong-plugin-agent-team`：10 通过、1 ignored。
- `cargo test -p tiangong-plugin-browser`：32 通过。
- 量化：`execute.rs` 4515 → 3728 行（-787）；本次提交整体 +161/-1737（净减约 1576 行）。

## 行为变化说明

1. **问号结尾的过程文本直接完成**：旧逻辑"看起来不像最终答复 → 进 Summary 再判断"删除；无义务时任何非空文本都是合法最终回复。有义务场景由门控兜底。
2. **空回复 / 合成占位符进入修复路径**（旧：空回复进 Summary；占位符进 Summary），预算共享 `MAX_TOOL_PROTOCOL_REPAIRS`。
3. **工具轮次达到 30 上限明确失败**（旧：ForceFinal 强制生成最终回复）。
4. **不再产生 `SummaryText` 流事件**：最终回复以 ReactText 流出 + 消息 upsert（phase=Summary）提交；Session 落盘格式不变。

## 遗留与后续

- 义务来源暂只有附件（第一批高置信度）；"明确要求执行命令""修改后必须验证"等义务待入口 API 明确后在任务 16/17 补充。
- 前端对 `SummaryText` 事件的依赖需在任务 18 真实场景验证时确认（保留变体即为该目的）。
- 任务 16：模型请求策略外置（压缩重试、Provider 退避、安全预算）；任务 17：审批下沉工具流水线。
