# 01 - 阶段状态机与循环骨架

## 目标

定义 `TurnPhase` 枚举和外层循环骨架，为后续 ReAct Loop 重构和总结阶段实现提供基础结构。

## 范围

- `crates/tiangong-core/src/react/engine.rs` — 新增 `TurnPhase` 枚举、外层循环骨架
- `crates/tiangong-core/src/core/mod.rs` — 调整 `MAX_ROUNDS` 常量

## 依赖

- 前置任务：无
- 后续任务：02, 03, 04, 05, 06, 07
- 可并行任务：06（Session 消息阶段标记可独立定义）
- 阻塞说明：无

## 任务

- [ ] 定义 `TurnPhase` 枚举：`Initial` / `ToolExecution` / `Summary`
- [ ] 定义循环控制常量：
  - `MAX_OUTER_ITERATIONS: u32 = 3`
  - `MAX_TOOL_ROUNDS: usize = 15`（替代现有 `MAX_ROUNDS: usize = 20`）
  - `SUB_AGENT_MAX_TOOL_ROUNDS: usize = 8`（替代现有 `SUB_AGENT_MAX_ROUNDS: usize = 10`）
  - `SUB_AGENT_MAX_OUTER_ITERATIONS: u32 = 2`
- [ ] 在 `execute_turn` 中搭建外层循环骨架：
  ```rust
  let mut outer_iteration: u32 = 0;
  'outer: loop {
      let mut phase = if outer_iteration == 0 {
          TurnPhase::Initial
      } else {
          TurnPhase::ToolExecution
      };

      // ReAct Loop（内层）— Task 02 实现
      // Summary Phase — Task 03 实现

      outer_iteration += 1;
      if outer_iteration >= MAX_OUTER_ITERATIONS {
          // force_final_response — Task 09 改进
          break;
      }
  }
  ```
- [ ] 保留现有的命令通道检查（cancel/message injection/approval）在外层循环入口处
- [ ] 确保 `execute_turn` 的函数签名不变，外部调用方无感知

## 不做

- 不实现 ReAct Loop 内部逻辑（Task 02）
- 不实现总结阶段逻辑（Task 03）
- 不移除 lite 模型检测（Task 04）
- 不修改 StreamEvent（Task 05）
- 不修改前端

## 验收

- `TurnPhase` 枚举已定义且可在 engine.rs 中使用
- 外层循环骨架存在，内层用 `todo!()` 或空实现占位
- 循环控制常量已定义且替换了旧常量
- `cargo check` 通过

## 验证

```bash
cargo check -p tiangong-core
```
