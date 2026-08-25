# DeepSeek Responses API 与 Files API 需求文档

## 背景

DeepSeek 官方 API 已原生支持 OpenAI Responses API 格式（为适配 Codex 等编码场景推出），并上线了 Files API 文件接口（支持图片上传后在对话中通过 `file_id` 引用）。当前 `tiangong-deepseek` SDK 仅覆盖 Chat Completions、Models、Balance 能力，需要扩展这两个新接口。

- 官方 Responses API 指南：https://api-docs.deepseek.com/zh-cn/guides/responses_api/
- 官方 Files API 指南：https://api-docs.deepseek.com/zh-cn/guides/files_api/

## 目标

在 `tiangong-deepseek` SDK 中新增 Responses API 与 Files API 的完整客户端支持，保持现有 crate 的模块组织方式（每个能力一个模块 + 对应类型文件 + 客户端能力访问器）。

## 功能需求

### 一、Responses API

端点：`POST /responses`，无状态（服务端不存储会话，多轮对话由客户端在 `input` 中回传完整历史）。

支持模型：`deepseek-v4-flash`、`deepseek-v4-pro`、`deepseek-v4-flash-vision-exp`。

1. **非流式请求**：构造请求（`model`、`input`、`instructions`、`stream`、`temperature`、`top_p`、`max_output_tokens`、`top_logprobs`、`tools`、`tool_choice`、`reasoning`、`text`、`user`）并解析响应。
   - `input` 支持消息项（message / function_call / function_call_output / reasoning / web_search_call），消息 content 支持字符串与 `input_text` / `output_text` / `input_image` 内容块。
   - `input_image` 支持 `image_url`（http(s) 或 base64 data URL）与 `file_id` 两种来源，二者互斥，并支持 `detail` 参数。
   - 响应结构与 OpenAI Responses API 兼容，`usage` 含 `input_tokens`（内含 `cached_tokens`）、`output_tokens`（内含 `reasoning_tokens`）。
2. **流式请求**：`stream: true` 时解析 SSE 事件序列（`response.created`、`response.output_item.added/done`、`response.output_text.delta/done`、`response.reasoning_text.delta/done`、`response.function_call_arguments.delta/done` 等），以 `response.completed` / `response.incomplete` / `response.failed` 结束（无 `[DONE]` 标记），事件含递增 `sequence_number`。
3. **参数边界**：不支持的参数（`previous_response_id`、`store`、`conversation`、`include`、`truncation` 等）不提供或标记为忽略；上下文超限服务端直接返回 400。

### 二、Files API

端点（OpenAI 兼容版，base_url `https://api.deepseek.com`）：

| 操作 | 端点 | 说明 |
|---|---|---|
| 上传文件 | `POST /files` | multipart/form-data，`file`（必填）+ `purpose`（必填，唯一取值 `user_data`）+ 可选 `expires_after[anchor]`（`created_at`）/ `expires_after[seconds]`（3600–2592000） |
| 列出文件 | `GET /files` | 支持 `after`、`limit`、`order`、`purpose` 参数 |
| 查询文件 | `GET /files/{file_id}` | 返回单个文件信息 |
| 删除文件 | `DELETE /files/{file_id}` | 删除指定文件 |

- 支持格式：JPEG、PNG、GIF、WebP（按文件实际内容判断）；单文件最大 64 MiB；文件名最长 512 字符。
- `file_id` 形如 `file-api-xxxxxxxxxxxxxxxx`，归属 API key，可在 Chat Completions 与 Responses API 的 `input_image` 中通过 `file_id` 引用（仅 `deepseek-v4-flash-vision-exp` 真正处理图片输入）。

## 改动范围

| 模块 | 改动 |
|---|---|
| `crates/tiangong-deepseek/src/client.rs` | 新增 `responses()`、`files()` 能力访问器；新增 multipart 上传与删除的通用 HTTP 方法 |
| `crates/tiangong-deepseek/src/responses.rs`（新增） | Responses API 请求构造、非流式与流式调用 |
| `crates/tiangong-deepseek/src/types/responses.rs`（新增） | Responses 请求/响应/SSE 事件类型 |
| `crates/tiangong-deepseek/src/files.rs`（新增） | Files API 四个操作 |
| `crates/tiangong-deepseek/src/types/files.rs`（新增） | 文件对象与列表类型 |
| `crates/tiangong-deepseek/src/error.rs` | 按需补充错误变体（如 multipart 构造失败） |

`tiangong-llm` provider 层暂不改动，本次仅扩展 SDK 能力（provider 是否切换到 Responses 接口由后续需求决定）。

## 验收标准

1. `cargo check` / `cargo clippy` 通过，现有测试不受影响。
2. Responses API：能构造非流式与流式请求，类型覆盖官方文档列出的字段；SSE 事件解析完整（含 delta 与终止事件）。
3. Files API：四个操作均有对应方法，上传支持 multipart 表单（含可选过期时间）。
4. 不引入新依赖（multipart 使用现有依赖实现；若确需新增依赖，先确认）。
