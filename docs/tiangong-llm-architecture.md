# tiangong-llm 架构约束

## 目的

本文档用于约束 `crates/tiangong-llm` 的职责边界，避免其从“统一 LLM 适配层”继续演化成“大而杂的协议实现仓库”。

当前原则是：

- `tiangong-llm` 负责统一抽象
- 各协议 provider 负责协议级映射与 provider 封装
- 具体 transport 尽量下沉到独立 crate
- 上层业务不得依赖第三方 SDK 类型

## 当前定位

`tiangong-llm` 是上层业务与具体 LLM 协议实现之间的稳定边界层，当前主要承担以下职责：

- 定义统一领域模型：
  - `crates/tiangong-llm/src/request.rs`
  - `crates/tiangong-llm/src/response.rs`
  - `crates/tiangong-llm/src/message.rs`
  - `crates/tiangong-llm/src/tool.rs`
  - `crates/tiangong-llm/src/stream.rs`
  - `crates/tiangong-llm/src/error.rs`
- 定义统一 provider trait：
  - `crates/tiangong-llm/src/provider.rs`
- 维护 provider 适配入口：
  - `crates/tiangong-llm/src/providers/anthropic`
  - `crates/tiangong-llm/src/providers/openai`（Responses API）
  - `crates/tiangong-llm/src/providers/openai_chatcompletions`（Chat Completions API）
  - `crates/tiangong-llm/src/providers/deepseek`
- 在 provider 内完成统一模型与协议模型之间的映射
- 在 provider 内完成统一错误模型与协议错误之间的映射

它不应直接承载上层业务决策，也不应成为 transport 细节、重试策略、产品规则的堆积点。

## 明确边界

### 可以放在 tiangong-llm 内的内容

- 与供应商无关的统一请求、响应、消息、工具、流事件、usage 和错误模型
- `LlmProvider` trait 与 provider capability 描述
- provider 入口对象与最薄一层 provider facade
- 请求映射、响应映射、流事件映射
- 供应商错误向统一错误的转换
- 与协议强相关、但仍然属于 provider 适配一部分的轻量兼容逻辑

### 不应继续放在 tiangong-llm 内的内容

- 供应商专属的底层 HTTP/SSE transport 细节
- 大量 `reqwest`/SDK client 构建代码
- 长段重试执行器、退避策略和日志模板的重复实现
- 与业务场景绑定的提示词策略、模型选择策略、thinking 开关策略
- `tiangong-core` 的运行时逻辑、消息拼装逻辑、任务编排逻辑
- 面向 GUI/CLI/Server 的展示层兼容逻辑

## 推荐结构

理想结构应保持为三层：

1. 上层调用层
   - `tiangong-core`
   - 只使用统一接口和统一领域模型
2. 统一适配层
   - `tiangong-llm`
   - 只负责抽象、映射、错误边界和 provider facade
3. 协议 transport 层
   - 例如 `crates/tiangong-anthropic`
   - 负责底层 HTTP、SSE、原始响应解析、协议模型

## 当前状态判断

当前实现中：

- Anthropic 链路相对符合目标结构
  - `crates/tiangong-anthropic` 承担了原生 transport
  - `tiangong-llm` 中 Anthropic provider 主要负责映射和封装
- OpenAI 链路分为 Responses（`providers/openai`）与 Chat Completions（`providers/openai_chatcompletions`）两套适配，仍处于过渡态
  - `crates/tiangong-llm/src/providers/openai_chatcompletions/provider.rs` 目前同时承担了 client 构建、重试、timeout、list models、错误分类等职责
  - 这部分后续应拆出独立 transport 或共享执行层，但当前不作为阻塞其他功能开发的前置任务

结论：

- `tiangong-llm` 当前可继续承载新增 provider 能力
- 但不得再复制 OpenAI 兼容当前这种“provider 内堆 transport 细节”的模式

## 新增 Provider 的规则

新增 provider 时必须遵循以下顺序：

1. 先定义统一边界是否已足够
2. 如需协议专属 transport，优先新建独立 crate
3. 在 `tiangong-llm` 中仅新增：
   - provider facade
   - mapping
   - error adapter
   - stream adapter
4. 不允许将第三方 SDK 类型暴露到 `tiangong-core`

## 后续重构优先级

以下事项属于后续收敛方向，但当前不阻塞其他主功能开发：

- 将 OpenAI transport 从 `crates/tiangong-llm/src/providers/openai_chatcompletions/provider.rs` 中拆出
- 抽取 provider 共用的 retry/logging 执行器
- 统一 `list_models` 的 transport 承载方式
- 继续缩减 provider 文件中的 HTTP 直连细节

## 变更准则

后续修改 `tiangong-llm` 时，优先遵循以下判断：

- 这是统一抽象问题，还是某个协议的 transport 问题
- 这是 provider 映射问题，还是 `tiangong-core` 的业务问题
- 这段逻辑是否会被多个 provider 复用
- 这段逻辑是否会让 `tiangong-llm` 对上层行为产生不必要的产品耦合

如果答案偏向 transport、业务策略或上层产品行为，则不应继续塞进 `tiangong-llm`。
