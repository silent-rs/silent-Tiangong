# TODO - 天工当前开发任务

> 最后更新：2026-08-12
> 当前主线：0.14.x OpenAI Responses 适配
> 参考：`PLAN.md`、`docs/requirements.md`

## OpenAI Responses 适配

- [x] 从最新主线恢复独立 Responses Provider
- [x] 支持同步与流式文本、思考摘要和 token 用量
- [x] 支持工具调用、工具结果回放和后续请求
- [x] 支持流式工具参数兜底和调用顺序保持
- [x] 将流式结束原因传递到统一响应
- [x] 完成 `tiangong-llm` 编译和测试
- [x] 完成上层核心模块编译与严格检查
- [x] 在前端供应商配置中增加 OpenAI Responses 协议选项
- [x] 完成前端构建验证

- [x] 修复交互式配置向导对 OpenAI Responses 协议的支持
- [x] 验证 OpenAI Responses 后台流式与服务端取消能力（因上游兼容性问题已回退）
- [x] 将 OpenAI Responses 改为普通流式模式，并通过关闭连接取消请求

## 编辑历史消息重发性能优化

- [x] 分析编辑重发慢的根因（每次销毁重建 Core）
- [x] edit_and_resend 改为复用 Core（去掉 retire_core + restore_session）
- [x] 调整 deliver 失败回滚（不销毁复用 Core，仅回滚磁盘 session）
- [x] 会话存在性校验改用 session_exists，消除全目录扫描
- [x] 前端 editAndResend 草稿同步改为后台非阻塞
- [x] 前端 handleConfirmEdit 去掉二次 structuredClone 深拷贝
- [x] 后端 cargo check / clippy 通过
- [x] 前端 build 通过

## 插件会话生命周期钩子修正

- [x] TiangongCore 增加 session_ready 状态，on_session_ready 改为每个 Core 实例只调一次
- [x] 系统提示保持每轮重建（不放进只执行一次判断）
- [x] 终端插件每轮终端状态注入从 on_session_ready 迁移到 on_turn_started
- [x] 修正 commands.rs / 文档中关于 on_session_ready 的错误描述
