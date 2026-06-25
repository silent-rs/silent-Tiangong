# 02 - Workspace Index 写入器失败修复

## 目标

修复 `index_search` 出现“创建 Workspace 索引写入器失败”的异常，恢复 Agent 对当前工作区的语义检索和符号检索能力。

## 范围

- `crates/tiangong-core/src/index/`
- `crates/tiangong-core/src/memory/` 中与 workspace index 接入相关的部分
- `crates/tiangong-memory/` 中与索引写入器、锁、目录初始化相关的部分
- 必要的错误日志、恢复逻辑和测试

## 依赖

- 前置任务：01
- 后续任务：依赖代码检索的后续修复任务
- 可并行任务：03
- 阻塞说明：如果 01 未完成，无法确认本修复属于哪个目标版本和当前主线。

## 任务

- 复现 `index_search` 写入器创建失败。
- 定位失败原因：目录权限、索引锁、损坏索引、重复 writer、workspace scope、长生命周期进程占用等。
- 改善错误信息，至少包含 workspace、索引目录、失败阶段和底层错误。
- 对可恢复异常提供恢复策略，例如自动重建索引或清理失效锁。
- 确保索引失败时主对话链路可降级继续运行。
- 增加针对索引初始化失败/恢复的测试或最小验证路径。
- 更新进度记录中的验证结果。

## 不做

- 不重写 Memory 系统。
- 不替换索引后端。
- 不新增前端索引管理页面。
- 不改变 `recall_memory` 的用户可见语义。
- 不把工具空参数问题放进本任务修复。

## 验收

- `index_search` 可以正常返回当前 workspace 的文件或符号结果。
- 索引目录异常时错误信息可诊断。
- 如果索引损坏或锁异常，系统有明确恢复路径。
- 主对话不会因为索引初始化失败而崩溃。
- 相关测试或手动验证通过。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- 如存在相关测试，运行 index/memory 相关测试。
- 手动调用 `index_search` 查询当前 workspace 中已知文件或符号。

## GitHub Issue

- Issue: #166
- PR 关闭关键字：`Closes #166`
