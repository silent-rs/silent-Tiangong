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
- [x] MemoryCandidate / MemoryCandidateKind / TurnMessage / EnhancedTurnResult 类型定义
- [x] ExtractionOutput / Evidence 类型定义
- [x] 候选提交 API（SubmitCandidate / RunEnhancedMicroRumination 命令 + Handle + IPC）
- [x] 工具执行候选评估（evaluate_tool_result_for_memory + build_enhanced_memory_turn_result）
- [x] Engine 工具执行循环中提交候选
- [x] core/mod.rs 轮次结束使用 run_enhanced_micro_rumination 替代 run_micro_rumination
- [x] 增强版 Micro 反刍管道（process_enhanced_micro + 去重 + 跨类型关联）
- [x] 多类型记忆提取（extract_multi_type_memories）
- [x] Episode 去重检查（check_episode_dedup：关键词重叠 ≥ 0.7 + 标题相似度 > 0.6）
- [x] 跨类型关联（Episode→Entity: BelongsTo，Decision→Episode: LearnedFrom）

---

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

## 文档同步要求

后续实现上述任一任务时，需要同步更新：

- `docs/requirements.md`
- `docs/memory-system/13-适时触发多类型记忆与去重.md` 状态区
