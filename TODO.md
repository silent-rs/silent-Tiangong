# TODO - 天工当前开发任务

> 最后更新：2026-05-10
> 当前主线：Memory 系统多类型记忆与智能去重
> 参考：`PLAN.md`、`docs/requirements.md`、`docs/memory-system/13-适时触发多类型记忆与去重.md`

---

## 已完成

- [x] 运行时粗回忆（RuntimeRecallPolicy / RuntimeRecallContext / RecallSufficiency 类型）
- [x] 粗回忆 Store 方法（rough_recall）
- [x] 粗回忆命令分发（RoughRecall / EvaluateRecallSufficiency）
- [x] 粗回忆 Handle 方法（async + blocking，含 IPC 支持）
- [x] 召回充分性评估规则（evaluate_recall_sufficiency）
- [x] 运行时重新回忆辅助（maybe_inject_runtime_memory_recall）
- [x] 用户消息进入后粗回忆触发
- [x] 工具调用前上下文充分性检查与重新回忆
- [x] 工具失败后重新回忆
- [x] 重新回忆结果作为运行时工具上下文进入消息链

---

## P0 - MemoryCandidate 与 EnhancedTurnResult 类型定义

- [ ] `tiangong-memory/src/types.rs` 新增 `MemoryCandidate` 结构体
  - 字段：tool_name、step_index、hint、suggested_kinds（Episode/Entity/Decision/Evidence/UserPreference）、file_path、url、result_summary、success
- [ ] `tiangong-memory/src/types.rs` 新增 `MemoryCandidateKind` 枚举
- [ ] `tiangong-memory/src/types.rs` 新增 `TurnMessage` 结构体（角色 + 内容）
- [ ] `tiangong-memory/src/types.rs` 新增 `EnhancedTurnResult` 结构体
  - 复用 TurnResult 原有字段 + memory_candidates: Vec\<MemoryCandidate\> + turn_messages: Vec\<TurnMessage\>

## P0 - 候选提交 API

- [ ] `tiangong-memory/src/command.rs` 新增 `MemoryCommand::SubmitCandidate { candidate, reply }`
- [ ] `tiangong-memory/src/command.rs` 新增 `MemoryCommand::RunEnhancedMicroRumination { turn_result, reply }`
- [ ] `tiangong-memory/src/actor.rs` 新增 `pending_candidates: Vec<MemoryCandidate>` 缓冲字段
- [ ] `tiangong-memory/src/actor.rs` SubmitCandidate 命令分发：追加到 pending_candidates
- [ ] `tiangong-memory/src/actor.rs` RunEnhancedMicroRumination 命令分发：合并 pending_candidates + EnhancedTurnResult，调用 process_enhanced_micro
- [ ] `tiangong-memory/src/handle.rs` 新增 `submit_memory_candidate`（fire-and-forget）
- [ ] `tiangong-memory/src/handle.rs` 新增 `run_enhanced_micro_rumination`（等待完成）
- [ ] `tiangong-memory/src/ipc/protocol.rs` 新增 IPC payload 变体：SubmitCandidate、RunEnhancedMicroRumination
- [ ] `tiangong-memory/src/ipc/mod.rs` 新增 IPC 请求处理

## P0 - 工具执行候选评估（tiangong-core 侧）

- [ ] `tiangong-core/src/memory/turn_result.rs` 新增 `evaluate_tool_result_for_memory(tool_name, success, result_summary, file_path) -> Option<MemoryCandidate>`
  - write_file / replace_in_file 成功 → Episode + Evidence
  - search_code 结果非空 → Entity
  - run_command 涉及构建/测试 → Episode + Decision
  - 其他 summary 超过 20 字符 → Episode
- [ ] `tiangong-core/src/memory/turn_result.rs` 新增 `build_enhanced_memory_turn_result(session, candidates) -> EnhancedTurnResult`
  - 复用现有 build_memory_turn_result 逻辑
  - 附加 memory_candidates 和 turn_messages
- [ ] `tiangong-core/src/react/engine.rs` 工具执行完成后调用 evaluate_tool_result_for_memory 并 submit_memory_candidate
- [ ] `tiangong-core/src/core/mod.rs` worker_loop_async 轮次结束调用 run_enhanced_micro_rumination 替代 run_micro_rumination

## P1 - 增强版 Micro 反刍管道

- [ ] `tiangong-memory/src/rumination.rs`（或新模块）新增 `process_enhanced_micro(store, turn_result, candidates)`
  - 调用 extract_multi_type_memories 提取多类型记忆
  - 调用 write_extraction_with_dedup 去重写入
  - 调用 link_written_memories 跨类型关联
  - 更新 session_injection
- [ ] Episode 去重检查 `check_episode_dedup(store, extraction) -> Option<existing_id>`
  - 关键词重叠 ≥ 0.7 + 标题相似度 > 0.6 → 更新已有
  - 否则新建
- [ ] 跨类型关联 `link_written_memories(store, written_ids, extraction)`
  - Episode → Entity: BelongsTo
  - Decision → Episode: LearnedFrom
  - Episode → Episode: RelatedTo（已有逻辑）

## P1 - 多类型记忆提取

- [ ] `tiangong-memory/src/writer.rs` 新增 `extract_multi_type_memories(store, enhanced_turn_result) -> ExtractionOutput`
  - 检查决策信号 → 内联提取 Decision
  - 检查实体信号 → 内联提取 Entity
  - 始终提取至少一个 Episode
- [ ] `ExtractionOutput` 类型定义：episodes、entities、decisions、evidences 及其字段

## P2 - Inline Entity/Decision 提取

- [ ] 轻量 LLM 提示从单轮中即时提取 Entity/Decision
  - Memory LLM 已配置时使用 LLM 提取
  - 未配置时走规则 fallback

## P2 - 集成验证

- [ ] 验证：用户问"继续上次那个模块"，粗回忆命中后提供文件/决策线索
- [ ] 验证：工具运行失败，重新回忆命中过往同类失败和修复命令
- [ ] 验证：简单普通问答不触发深度混合检索
- [ ] 验证：多类型记忆写入后跨类型关联正确建立
- [ ] 验证：相似记忆去重为更新而非新建

---

## 推荐执行顺序

1. P0 类型定义（MemoryCandidate / EnhancedTurnResult / ExtractionOutput）
2. P0 候选提交 API（Command / Actor / Handle / IPC）
3. P0 工具执行候选评估（tiangong-core 侧 evaluate + submit + engine 集成）
4. P1 增强版 Micro 反刍管道（process_enhanced_micro + 去重 + 关联）
5. P1 多类型记忆提取（extract_multi_type_memories）
6. P2 Inline Entity/Decision 提取
7. P2 集成验证

---

## 文档同步要求

后续实现上述任一任务时，需要同步更新：

- `docs/requirements.md`
- `docs/memory-system/13-适时触发多类型记忆与去重.md` 状态区
