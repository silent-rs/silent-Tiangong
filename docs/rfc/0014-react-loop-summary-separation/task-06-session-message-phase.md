# 06 - Session 消息阶段标记

## 目标

在 Session 的 Message 结构中新增 `phase` 字段，标记消息属于 ReAct Loop 阶段还是总结阶段，支持前端消息分层展示和向后兼容。

## 范围

- `crates/tiangong-core/src/session.rs` — Message 结构新增 `phase` 字段
- `crates/tiangong-core/src/react/message.rs` — 消息追加函数适配 phase 标记

## 依赖

- 前置任务：无（字段定义可独立完成）
- 后续任务：02, 03（使用 phase 标记）
- 可并行任务：01, 02, 03
- 阻塞说明：无

## 任务

- [ ] 定义 `MessagePhase` 枚举：
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "lowercase")]
  pub enum MessagePhase {
      /// 默认值：旧消息或未标记消息（向后兼容）
      #[default]
      Normal,
      /// ReAct Loop 中的消息
      React,
      /// 总结阶段的最终回复
      Summary,
  }
  ```
- [ ] 在 `Message` 结构中新增字段：
  ```rust
  #[serde(default)]
  pub phase: MessagePhase,
  ```
- [ ] 确保 `serde` 反序列化旧 Session 时，`phase` 字段缺失时默认为 `Normal`
- [ ] 更新 `Message::new()` 等构造函数，`phase` 默认为 `Normal`
- [ ] 新增辅助构造方法：
  ```rust
  impl Message {
      pub fn with_phase(mut self, phase: MessagePhase) -> Self {
          self.phase = phase;
          self
      }
  }
  ```
- [ ] 在 `append_assistant_tool_call_message` 中标记 `phase: MessagePhase::React`
- [ ] 在 `append_tool_result_message` 中标记 `phase: MessagePhase::React`
- [ ] 总结阶段的消息追加标记 `phase: MessagePhase::Summary`（由 Task 03 调用方设置）
- [ ] `force_final_response` 产出的消息标记 `phase: MessagePhase::Summary`
- [ ] Sub Agent 的 `sub_agent_stream_message` 标记合适的 phase

## 不做

- 不修改前端（Task 08）
- 不修改 StreamEvent（Task 05）
- 不改变 Session 持久化格式（新增字段通过 serde default 向后兼容）
- 不迁移旧消息的 phase（旧消息保持 `Normal`）

## 验收

- `MessagePhase` 枚举已定义
- `Message` 结构包含 `phase` 字段，serde 默认值为 `Normal`
- 旧 Session JSON 反序列化不报错，`phase` 自动填充为 `Normal`
- ReAct Loop 中的消息标记为 `React`
- 总结阶段的消息标记为 `Summary`
- `cargo check` 通过
- `cargo test -p tiangong-core` 通过

## 验证

```bash
cargo check -p tiangong-core
cargo test -p tiangong-core
# 手动验证：加载旧 Session，确认不报错
```
