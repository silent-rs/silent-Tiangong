# 04 - 移除 lite 模型完成度检测

## 目标

移除 `check_completion_with_lite_model` 及所有相关代码，完成度判断由总结阶段的主模型接管。

## 范围

- `crates/tiangong-core/src/react/context.rs` — 移除 `check_completion_with_lite_model`、`COMPLETION_CHECK_SYSTEM_PROMPT`、`format_completion_check_prompt`、`parse_completion_response`
- `crates/tiangong-core/src/react/engine.rs` — 移除 `completion_check_count` 变量及相关分支逻辑

## 依赖

- 前置任务：01, 02, 03
- 后续任务：无
- 可并行任务：05, 06
- 阻塞说明：需要 Task 03 的总结阶段已能正常判断完成度，才能安全移除 lite 模型检测

## 任务

- [ ] 移除 `context.rs` 中的以下内容：
  - `COMPLETION_CHECK_SYSTEM_PROMPT` 常量
  - `check_completion_with_lite_model` 函数
  - `format_completion_check_prompt` 函数
  - `parse_completion_response` 函数
- [ ] 移除 `engine.rs` 中 `execute_turn` 内的以下内容：
  - `completion_check_count` 变量声明
  - `if round < self.max_rounds && completion_check_count < 2` 分支
  - INCOMPLETE 时注入的 system-reminder 消息
  - 相关的 `continue 'react_loop` 逻辑
- [ ] 清理 `engine.rs` 中对 `check_completion_with_lite_model` 的 import
- [ ] 确认 `lite_client` 仍被其他功能使用（标题生成等），不要移除 `lite_client` 本身
- [ ] 搜索全项目确认无残留引用

## 不做

- 不移除 `lite_client`（仍用于标题生成等）
- 不修改总结阶段逻辑（Task 03 已完成）
- 不修改 StreamEvent
- 不修改前端

## 验收

- `check_completion_with_lite_model` 及相关函数/常量已完全移除
- `completion_check_count` 变量已移除
- 全项目搜索 `check_completion` / `completion_check` / `COMPLETION_CHECK` 无结果
- `lite_client` 仍存在且被标题生成等功能正常使用
- `cargo check` 通过
- `cargo test -p tiangong-core` 通过

## 验证

```bash
# 确认无残留引用
rg "check_completion|completion_check|COMPLETION_CHECK" crates/
# 编译检查
cargo check -p tiangong-core
# 测试
cargo test -p tiangong-core
```
