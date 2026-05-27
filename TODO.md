# TODO - 天工当前开发任务

> 最后更新：2026-05-27
> 当前主线：Phase 20 — 自动化触发层（0.4.0）✅ 已完成
> 参考：`PLAN.md`、Issue #38

---

## Phase 20-A：任务模型与存储

- [x] 定义 Job 数据模型（id / name / trigger_type / schedule / payload / enabled / created_at / updated_at）
- [x] 定义 JobRun 数据模型（id / job_id / status / started_at / finished_at / result）
- [x] 定义 JobDelivery 数据模型（id / job_run_id / channel / status / retry_count）
- [x] 实现 SQLite job store（建表、CRUD）
- [x] 实现 run history 存储（写入、查询、状态更新）
- [x] 新增 Job CRUD API（`POST/GET/PUT/DELETE /api/v1/jobs`）

## Phase 20-B：Cron 调度器

- [x] 引入 cron 表达式解析库（如 `cron` 或 `saffron`）
- [x] 实现 Scheduler 常驻执行器（tokio spawn，内嵌于 Server 启动流程）
- [x] Cron job 触发 → 构造 RuntimeEvent → 进入现有执行链路
- [x] Server 启动时从 job store 加载已启用的 cron job 并恢复调度
- [x] 支持执行目标指定（main session / isolated session / skill）
- [x] 手动触发 API（`POST /api/v1/jobs/:id/trigger`）

## Phase 20-C：Webhook 触发器

- [x] Webhook 端点注册（每个 webhook job 分配唯一路径）
- [x] 实现 `POST /api/v1/webhooks/:token` 端点
- [x] 请求签名验证（HMAC-SHA256）
- [x] Webhook 触发 → 构造 RuntimeEvent → 进入现有执行链路

## Phase 20-D：Polling 触发器

- [x] 实现 HTTP polling 轮询执行器（定时请求指定 URL）
- [x] 条件判断与去重（响应内容变化时才触发）
- [x] Polling 触发 → 构造 RuntimeEvent → 进入现有执行链路

## Phase 20-E：结果投递与通知

- [x] 执行结果投递到 IM 通道（复用 `POST /api/v1/messages` 或直接调用 MessageRouter）
- [x] 失败重试机制（可配置重试次数与间隔）
- [x] Run history 查询 API（`GET /api/v1/jobs/:id/runs`）
- [x] 投递状态追踪与查询

## Phase 20-F：前端管理界面

- [x] Job 列表页（展示所有 job、状态、下次执行时间）
- [x] Job 创建/编辑表单（cron 表达式、webhook URL、polling 配置）
- [x] Job 启停开关
- [x] 执行历史查看（run 列表、状态、耗时、结果摘要）
- [x] 手动触发按钮

## 发布准备（0.4.0）

- [x] 更新 `Cargo.toml` 版本号为 `0.4.0`
- [x] 更新 `tauri.conf.json` 版本号
- [x] 验证 cron / webhook / polling 端到端流程
- [x] 验证 Server 启动恢复 cron job
- [x] 验证前端 Job 管理界面

---

## 文档同步要求

- `docs/requirements.md`：补充自动化触发层相关需求
- `docs/server-api.md`：补充 Job / Webhook API 文档
- Issue #38：开发完成后关闭
