# TODO - 天工当前开发任务

> 最后更新：2026-05-25
> 当前主线：Phase 20 — 自动化触发层（0.4.0）
> 参考：`PLAN.md`、Issue #38

---

## Phase 20-A：任务模型与存储

- [ ] 定义 Job 数据模型（id / name / description / trigger_type / schedule / session_id / payload / enabled / created_at / updated_at）
  - `trigger_type`：Cron / Webhook / Polling
  - `session_id`：可选，指定则在关联 session 中执行，否则自动创建新 session
  - `payload`：触发时构造给 LLM 的任务描述模板
- [ ] 定义 JobRun 数据模型（id / job_id / session_id / status / started_at / finished_at / result_summary）
- [ ] 在 tiangong-server 中引入 SQLite 依赖（rusqlite 或 sqlx）
- [ ] 实现 Job store（建表、CRUD、按 trigger_type 查询）
- [ ] 实现 JobRun store（写入、按 job_id 查询、状态更新）
- [ ] 新增 Job CRUD API（`POST/GET/PUT/DELETE /api/v1/jobs`）
- [ ] 新增 JobRun 查询 API（`GET /api/v1/jobs/:id/runs`）

## Phase 20-B：Cron 调度器

- [ ] 为 tiangong-server 启用 silent 的 `scheduler` feature
- [ ] 实现 Job → silent Task 转换（将 Job 的 schedule 映射为 ProcessTime，payload 映射为 action）
- [ ] action 内部：查找或创建 session → 构造用户消息 → 调用 RuntimeEngine::execute_turn_with_streaming
- [ ] action 内部：记录 JobRun（状态、耗时、结果摘要）
- [ ] Server 启动时从 job store 加载已启用的 cron job 并注册到 silent scheduler
- [ ] Job 启停时同步增删 silent Task
- [ ] 手动触发 API（`POST /api/v1/jobs/:id/trigger`）

## Phase 20-C：Webhook 触发器

- [ ] Webhook 端点注册（每个 webhook job 分配唯一 token）
- [ ] 实现 `POST /api/v1/webhooks/:token` 端点
- [ ] 请求签名验证（HMAC-SHA256）
- [ ] Webhook 触发 → 查找或创建 session → 构造用户消息（含 webhook payload）→ RuntimeEngine 执行

## Phase 20-D：Polling 触发器

- [ ] 实现 HTTP polling 轮询执行器（基于 silent scheduler 或独立 tokio spawn）
- [ ] 条件判断与去重（响应内容变化时才触发）
- [ ] Polling 触发 → 查找或创建 session → 构造用户消息（含 polling 响应）→ RuntimeEngine 执行

## Phase 20-E：执行记录与历史查询

- [ ] JobRun 执行记录完善（成功/失败状态、错误信息、结果摘要）
- [ ] Run history 查询 API 完善（分页、按状态过滤、按时间排序）
- [ ] 手动触发时同步记录 JobRun

## Phase 20-F：前端管理界面

- [ ] Job 列表页（展示所有 job、状态、下次执行时间）
- [ ] Job 创建/编辑表单（trigger_type 切换、cron 表达式、webhook 配置、polling 配置、session 选择）
- [ ] Job 启停开关
- [ ] 执行历史查看（run 列表、状态、耗时、结果摘要）
- [ ] 手动触发按钮

## 发布准备（0.4.0）

- [ ] 更新 `Cargo.toml` 版本号为 `0.4.0`
- [ ] 更新 `tauri.conf.json` 版本号
- [ ] 更新 `CHANGELOG.md`
- [ ] 验证 cron / webhook / polling 端到端流程
- [ ] 验证 Server 启动恢复 cron job
- [ ] 验证前端 Job 管理界面

---

## 文档同步要求

- `docs/requirements.md`：补充自动化触发层相关需求
- `docs/server-api.md`：补充 Job / Webhook API 文档
- Issue #38：开发完成后关闭
