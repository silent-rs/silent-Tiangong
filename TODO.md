# TODO - 天工当前开发任务

> 最后更新：2026-05-25
> 当前主线：Phase 20 — 自动化触发层（0.4.0）
> 参考：`PLAN.md`、Issue #38

---

## Phase 20-A：任务模型与存储

- [x] 定义 Job 数据模型（id / name / description / trigger_type / schedule / session_id / payload / enabled / created_at / updated_at）
  - `trigger_type`：Cron（仅定时任务）
  - `session_id`：可选，指定则在关联 session 中执行，否则自动创建新 session
  - `payload`：触发时构造给 LLM 的任务描述模板
- [x] 定义 JobRun 数据模型（id / job_id / session_id / status / started_at / finished_at / result_summary）
- [x] 创建 tiangong-scheduler 独立 crate，使用 JSON 文件存储（`~/.tiangong/scheduler/`）
- [x] 实现 Job store（CRUD）
- [x] 实现 JobRun store（写入、按 job_id 查询、状态更新）
- [x] 新增 Job CRUD API（`POST/GET/PUT/DELETE /api/v1/jobs`）
- [x] 新增 JobRun 查询 API（`GET /api/v1/jobs/:id/runs`）

## Phase 20-B：Cron 调度器

- [x] 为 tiangong-server 启用 silent 的 `scheduler` feature
- [x] 实现 Job → silent Task 转换（将 Job 的 schedule 映射为 ProcessTime，payload 映射为 action）
- [x] action 内部通过 executor 走完整执行链路
- [x] Server 启动时从 job store 加载已启用的 cron job 并注册到 silent scheduler
- [x] 手动触发 API（`POST /api/v1/jobs/:id/trigger`）

## Phase 20-C：Webhook 触发器（Server 内置能力）

- [x] 创建独立 Webhook 模块（model + store），独立于 scheduler crate
- [x] Webhook JSON 存储（`~/.tiangong/webhooks/`）
- [x] Webhook CRUD API（`POST/GET/PUT/DELETE /api/v1/webhooks`）
- [x] 外部触发端点（`POST /api/v1/webhooks/:id/invoke`，无需认证，走 secret 签名验证）
- [x] 手动触发端点（`POST /api/v1/webhooks/:id/trigger`，需认证）
- [x] WebhookRun 执行记录和查询 API（`GET /api/v1/webhooks/:id/runs`）
- [x] 请求签名验证（通过 X-Webhook-Signature header）
- [x] Webhook 触发 → executor 走完整执行链路

## Phase 20-D：Polling 触发器（暂缓）

- [ ] 实现 HTTP polling 轮询执行器（独立 tokio spawn 后台任务）
- [ ] 条件判断与去重（响应内容变化时才触发）
- [ ] Polling 触发 → executor 走完整执行链路
- [ ] Server 启动时恢复 polling job

## Phase 20-E：Executor 通用化

- [x] 提取通用 ExecuteParams（trigger_id / trigger_name / trigger_description / session_id / payload）
- [x] 抽象 RunTracker（Job / Webhook 两种执行记录追踪方式）
- [x] 统一 execute() 核心函数，Job 和 Webhook 共享执行逻辑
- [x] resolve_session 通用化（支持可选 session_id，不存在时自动创建）

## Phase 20-F：前端管理界面

- [ ] Job 列表页（展示所有定时任务、状态、cron 表达式）
- [ ] Job 创建/编辑表单（cron 表达式、session 选择、payload 编辑）
- [ ] Job 启停开关
- [ ] Job 执行历史查看（run 列表、状态、耗时、结果摘要）
- [ ] Job 手动触发按钮
- [ ] Webhook 列表页（展示所有 webhook、secret 状态、启用状态）
- [ ] Webhook 创建/编辑表单（secret 配置、session 选择、payload 编辑）
- [ ] Webhook 调用地址展示（`/api/v1/webhooks/:id/invoke`）
- [ ] Webhook 执行历史查看

## 发布准备（0.4.0）

- [ ] 更新 `Cargo.toml` 版本号为 `0.4.0`
- [ ] 更新 `tauri.conf.json` 版本号
- [ ] 更新 `CHANGELOG.md`
- [ ] 验证 cron 端到端流程
- [ ] 验证 webhook 端到端流程
- [ ] 验证 Server 启动恢复 cron job
- [ ] 验证前端 Job / Webhook 管理界面

---

## 文档同步要求

- `docs/requirements.md`：补充自动化触发层相关需求
- `docs/server-api.md`：补充 Job / Webhook API 文档
- Issue #38：开发完成后关闭
