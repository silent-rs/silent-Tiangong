# TODO - 天工当前开发任务

> 最后更新：2026-05-11
> 当前主线：Memory 系统多类型记忆与智能去重
> 参考：`PLAN.md`、`docs/requirements.md`、`docs/memory-system/13-适时触发多类型记忆与去重.md`

---

## 已完成

- [x] 运行时粗回忆与召回充分性评估
- [x] 用户消息进入、工具调用前、工具失败后的重新回忆
- [x] 重新回忆结果作为运行时工具上下文进入消息链
- [x] MemoryCandidate / EnhancedTurnResult / ExtractionOutput 等类型定义
- [x] 候选提交 API（SubmitCandidate / RunEnhancedMicroRumination + Handle + IPC）
- [x] 工具执行候选评估与 Engine 集成
- [x] 增强版 Micro 反刍管道（多类型提取 + 去重 + 跨类型关联）
- [x] 多类型记忆提取由 Memory LLM 判断（含 Inline Entity/Decision）
- [x] Episode 去重（关键词重叠 ≥ 0.7 + 标题相似度 > 0.6）
- [x] 跨类型关联（Episode→Entity: BelongsTo，Decision→Episode: LearnedFrom）
- [x] 修复 async runtime 中 blocking_send panic

---

## 待验证

- [ ] 用户问"继续上次那个模块"，粗回忆命中后提供文件/决策线索
- [ ] 工具运行失败，重新回忆命中过往同类失败和修复命令
- [ ] 简单普通问答不触发深度混合检索
- [ ] 多类型记忆写入后跨类型关联正确建立
- [ ] 相似记忆去重为更新而非新建

---

## 文档同步要求

- `docs/requirements.md`
- `docs/memory-system/13-适时触发多类型记忆与去重.md` 状态区
