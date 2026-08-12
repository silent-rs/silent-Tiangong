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
- [x] 为 OpenAI Responses 增加后台流式与服务端取消能力
