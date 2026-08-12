# TODO - 天工当前开发任务

> 最后更新：2026-08-12
> 当前主线：0.14.x OpenAI Responses 适配、Fs 0.1.1 Windows 文件处理修复
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

## OpenAI 工具调用容错

- [x] 修复流式工具参数被提前判定完整并丢失后续增量的问题
- [x] 对同一响应内的工具调用逐项校验，保留合法调用、剔除异常调用并反馈 schema 原因
- [x] 移除工具参数自动重发和固定错误回复，全部异常时继续现有对话循环
- [x] 确保工具调用容错不会把多次请求用量累计为当前上下文大小
- [x] 完成模型层与核心流程测试、编译和严格检查

## Fs 0.1.1 Windows 文件处理修复

- [x] 创建 CSV 并验证线上 Fs 0.1.0 对多种 Windows 路径形式的读取行为
- [x] 复现 CRLF 文件多行替换和统一补丁失败
- [x] 修复已发送附件因 Windows 路径表示不同而被误清理
- [x] 修复 CRLF 文件的多行替换和统一补丁处理
- [x] 将 Fs 插件各组件版本统一更新为 0.1.1
- [x] 使用 CSV、TXT、JSON、空文件、较大文本和图片完成 Windows 回归验证
- [x] 验证 LF/CRLF 双向适配，并在 Linux、macOS、Windows 发布构建前运行文件回归
- [x] 完成格式、测试、严格检查、插件校验和完整构建
- [ ] 提交并推送修复分支，创建 PR 并指派 hubertshelley
- [ ] 合并 PR，发布并核验 plugin/fs/v0.1.1 三平台制品
