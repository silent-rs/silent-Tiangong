//! Memory System 的 WASM 桥接组件。
//!
//! 本组件只做桥接：不承载 memory 纯逻辑（规划/提取/整理），不做内部编排。
//! 全部 memory 处理由 memory sidecar 完成（含 LLM/检索/存储的完整能力）。
//!
//! 工作方式：
//! - handle-tool(recall_memory) 时，经通用 sidecar host import
//!   把请求转发到 sidecar，结果原样返回。
//! - sidecar 不可用时，返回明确提示。
//!
//! 见 issue #321 / RFC docs/memory-system/11-memory-sidecar-wasm-bridge.md。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use serde::Deserialize;
use tiangong_plugin_memory_protocol::injection::{LoadInjection, LoadInjectionRequest};
use tiangong_plugin_memory_protocol::recall::{
    Recall, RecallContext, RecallContextRequest, RecallQuery, RecallRequest,
};
use tiangong_plugin_memory_protocol::rumination::{
    EnhancedTurnResult, MemoryCandidate, RunEnhancedMicroRumination,
    RunEnhancedMicroRuminationRequest, RunMesoRumination, RunMesoRuminationRequest, TurnArtifact,
    TurnArtifactKind, TurnMessage,
};
use tiangong_plugin_memory_protocol::ui::{self, UiRequest};
use tiangong_plugin_memory_protocol::{Empty, MemoryOperation};

mod descriptor {
    pub const ID: &str = tiangong_plugin_memory_protocol::PLUGIN_ID;
    pub const NAME: &str = "Memory";
    pub const VERSION: &str = tiangong_plugin_memory_protocol::PLUGIN_VERSION;
}

#[derive(Debug, Default, Deserialize)]
struct RecallToolArguments {
    query: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    expected: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
/// 存放 prompt_sections 拉注入所需的 session_id 和 workspace。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        session_id: Option<String>,
        workspace: Option<String>,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState {
            session_id: None,
            workspace: None,
        }) };
    }

    pub fn set_session_id(id: Option<String>) {
        STATE.with(|s| s.borrow_mut().session_id = id);
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn session_id() -> Option<String> {
        STATE.with(|s| s.borrow().session_id.clone())
    }

    /// workspace_id = workspace 路径的末尾目录名（与原生 memory 一致）。
    pub fn workspace_id() -> Option<String> {
        STATE.with(|s| {
            s.borrow().workspace.as_ref().and_then(|ws| {
                ws.rsplit('/')
                    .next()
                    .filter(|n| !n.is_empty())
                    .map(String::from)
            })
        })
    }
}

/// recall_memory 工具的 input_schema（JSON 文本）。
/// 与进程内版本 `tiangong-plugin-memory/src/handler.rs` 保持一致。
const RECALL_MEMORY_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
    },
    "reason": {
      "type": "string",
      "description": "为什么需要回忆，简述当前任务依赖的历史语境"
    },
    "expected": {
      "type": "array",
      "items": { "type": "string" },
      "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
    },
    "limit": {
      "type": "integer",
      "description": "最多返回多少条记忆，默认 5，最大 10"
    }
  },
  "required": ["query"]
}"#;

const RECALL_MEMORY_DESCRIPTION: &str = "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。";

const MEMORY_PAGE_TEMPLATE: &str = include_str!("memory.html");
const MEMORY_PAGE_CSS: &str = include_str!("memory.css");
const MEMORY_PAGE_JS: &str = include_str!("memory.js");

fn memory_settings_html() -> String {
    MEMORY_PAGE_TEMPLATE
        .replace("/*__MEMORY_CSS__*/", MEMORY_PAGE_CSS)
        .replace("/*__MEMORY_JS__*/", MEMORY_PAGE_JS)
}

/// WASM 桥接组件（无状态）。
struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: descriptor::ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: descriptor::VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(vec![ToolSpec {
            name: "recall_memory".to_string(),
            description: RECALL_MEMORY_DESCRIPTION.to_string(),
            input_schema: RECALL_MEMORY_INPUT_SCHEMA.to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        // 读缓存的状态，经 request 拉取三级记忆注入。
        let session_id = state::session_id().unwrap_or_default();
        let workspace_id = state::workspace_id();
        let request = LoadInjectionRequest {
            session_id,
            workspace_id,
        };
        let sections = match sidecar_client::invoke::<LoadInjection>(&request) {
            Ok(response) => response.items,
            Err(_) => {
                // sidecar 不可用时返回空（不注入），不阻断 prompt 装配。
                Vec::new()
            }
        };
        Ok(sections)
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        if call.name != "recall_memory" {
            return Err(PluginError::Message(format!(
                "memory 组件不支持工具: {}",
                call.name
            )));
        }

        let arguments =
            serde_json::from_str::<RecallToolArguments>(&call.arguments).map_err(|error| {
                PluginError::Message(format!("解析 recall_memory 参数失败: {error}"))
            })?;
        let request = RecallContextRequest {
            request: RecallQuery {
                query: arguments.query,
                reason: arguments.reason,
                expected: arguments.expected,
                context: Vec::new(),
                limit: arguments.limit.unwrap_or(5),
            },
        };

        match sidecar_client::invoke::<RecallContext>(&request) {
            Ok(result) => Ok(tool_result_ok(result.response.content)),
            Err(sidecar_client::ClientError::NotConfigured)
            | Err(sidecar_client::ClientError::Unavailable(_)) => Ok(tool_result_ok(
                "记忆系统未启用（memory sidecar 未连接）。".to_string(),
            )),
            Err(error) => Ok(tool_result_ok(format!("记忆查询失败：{error}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>) -> Result<(), PluginError> {
        state::set_workspace(workspace);
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        // 通用配置变更事件。桥接组件本身不消费配置，接收即可。
        Ok(())
    }

    // ── 生命周期钩子 ──
    //
    // session-json 为宿主 Session 的只读快照（可序列化部分）。
    // WASM 从中提取 memory 需要的数据，经 request 转发到 sidecar。
    // session 的所有修改权始终在 Core，WASM/ sidecar 绝不回写。

    fn on_session_ready(session_json: String) -> Result<(), PluginError> {
        // 会话就绪：从 PluginSession 提取 id 缓存，供 prompt_sections 拉注入用。
        if let Ok(session) = serde_json::from_str::<tiangong_types::PluginSession>(&session_json) {
            state::set_session_id(Some(session.id));
        }
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        // 轮次开始：当前无需通知 sidecar。
        Ok(())
    }

    fn on_turn_finished(session_json: String, turn_start_idx: u32) -> Result<(), PluginError> {
        // 轮次结束：从 session 只读快照提取本轮信息，转发给 sidecar 做 micro 反刍。
        // 提取失败（session 格式异常）仅记录，不阻断——反刍是 best-effort。
        let _ = forward_turn_rumination(&session_json, turn_start_idx);
        Ok(())
    }

    fn on_session_ended(session_json: String) -> Result<(), PluginError> {
        // 会话结束：从 session 提取 id/cwd，转发给 sidecar 做 meso 反刍。
        let _ = forward_session_rumination(&session_json);
        Ok(())
    }
}

// ── UI 能力（plugin-ui 接口），独立于 Core 使用的 plugin 接口 ──

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(vec![Contribution {
            id: "memory".to_string(),
            title: "记忆".to_string(),
            description: "记忆系统配置（模型端点、向量检索等）".to_string(),
            icon: "brain".to_string(),
            group: "plugins".to_string(),
            has_view: true,
        }])
    }

    fn open_view(contribution_id: String) -> Result<ViewResponse, PluginError> {
        if contribution_id != "memory" {
            return Err(PluginError::Message(format!(
                "未知的 contribution: {contribution_id}"
            )));
        }
        Ok(ViewResponse {
            html: memory_settings_html(),
        })
    }

    fn get_view_resource(path: String) -> Result<ResourceResponse, PluginError> {
        match path.as_str() {
            "memory.css" => Ok(ResourceResponse {
                data: MEMORY_PAGE_CSS.as_bytes().to_vec(),
                mime: "text/css; charset=utf-8".to_string(),
            }),
            "memory.js" => Ok(ResourceResponse {
                data: MEMORY_PAGE_JS.as_bytes().to_vec(),
                mime: "text/javascript; charset=utf-8".to_string(),
            }),
            _ => Err(PluginError::Message(format!("无此资源: {path}"))),
        }
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "bootstrap" => invoke_for_ui::<ui::GetConfig>(&Empty {}),
            "save_config" => {
                let selection = serde_json::from_str::<ui::MemorySelection>(&request.payload)
                    .map_err(|error| {
                        PluginError::Message(format!("解析 Memory 配置失败: {error}"))
                    })?;
                invoke_for_ui::<ui::SetConfig>(&selection)
            }
            "memory_request" => forward_memory_ui_request(&request.payload),
            other => Err(PluginError::Message(format!("未知消息: {other}"))),
        }?;
        Ok(ViewMessageResponse { payload })
    }
}

fn invoke_for_ui<O>(request: &O::Request) -> Result<String, PluginError>
where
    O: MemoryOperation,
    O::Response: serde::Serialize,
{
    let response = sidecar_client::invoke::<O>(request)
        .map_err(|error| PluginError::Message(error.to_string()))?;
    serde_json::to_string(&response)
        .map_err(|error| PluginError::Message(format!("序列化 {} 响应失败: {error}", O::NAME)))
}

fn forward_memory_ui_request(payload: &str) -> Result<String, PluginError> {
    let request = serde_json::from_str::<UiRequest>(payload)
        .map_err(|error| PluginError::Message(format!("解析 Memory 页面请求失败: {error}")))?;
    match request {
        UiRequest::ListNodes { query } => {
            invoke_for_ui::<ui::ListNodes>(&ui::ListNodesRequest { query })
        }
        UiRequest::CountNodes { query } => {
            invoke_for_ui::<ui::CountNodes>(&ui::CountNodesRequest { query })
        }
        UiRequest::ListRelations { node_id } => {
            invoke_for_ui::<ui::ListRelations>(&ui::ListRelationsRequest { node_id })
        }
        UiRequest::ListRelationsBatch { node_ids } => {
            invoke_for_ui::<ui::ListRelationsBatch>(&ui::ListRelationsBatchRequest { node_ids })
        }
        UiRequest::UpsertManualMemory { draft } => {
            invoke_for_ui::<ui::UpsertManualMemory>(&ui::UpsertManualMemoryRequest { draft })
        }
        UiRequest::SetNodeStatus { node_id, status } => {
            invoke_for_ui::<ui::SetNodeStatus>(&ui::SetNodeStatusRequest { node_id, status })
        }
        UiRequest::UpsertRelation { draft } => {
            invoke_for_ui::<ui::UpsertRelation>(&ui::UpsertRelationRequest { draft })
        }
        UiRequest::DeleteRelation { relation_id } => {
            invoke_for_ui::<ui::DeleteRelation>(&ui::DeleteRelationRequest { relation_id })
        }
        UiRequest::Recall { anchors, limit } => {
            invoke_for_ui::<Recall>(&RecallRequest { anchors, limit })
        }
    }
}

/// 从 session 快照提取本轮信息，转发给 sidecar 做 enhanced micro 反刍。
///
/// 从 PluginSession 提取本轮完整内容，组装成 EnhancedTurnResult 转发给 sidecar。
///
/// 提取内容：
/// - 用户输入文本
/// - 助手最终回复作为 summary
/// - Assistant 消息中的 tool_calls
/// - Tool 消息的结果正文、是否失败
/// - 文件路径、URL、媒体等产物
/// - 本轮完整 turn_messages
/// - 基于工具结果生成 memory_candidates
fn forward_turn_rumination(session_json: &str, turn_start_idx: u32) -> Result<(), PluginError> {
    let session: tiangong_types::PluginSession = serde_json::from_str(session_json)
        .map_err(|e| PluginError::Message(format!("解析 PluginSession 失败: {e}")))?;

    let idx = turn_start_idx as usize;
    let messages = &session.messages;

    // 校验起点索引和角色。
    let start_msg = match messages.get(idx) {
        Some(m) if matches!(m.role, tiangong_types::MessageRole::User) => m,
        _ => {
            tracing_warn(&format!(
                "turn_start_idx={idx} 不存在或不是用户消息，跳过反刍"
            ));
            return Ok(());
        }
    };

    let user_input = extract_message_text(start_msg);
    let turn_messages = &messages[idx..];

    // 提取工具调用名（来自 Assistant 消息的 tool_calls 字段）。
    let tool_calls: Vec<String> = turn_messages
        .iter()
        .flat_map(|m| m.tool_calls.iter().map(|c| c.name.clone()))
        .collect();

    // 提取助手最终回复作为 summary。
    let summary = turn_messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, tiangong_types::MessageRole::Assistant))
        .map(extract_message_text)
        .unwrap_or_else(|| user_input.clone());

    // 提取产物和候选。
    let mut artifacts = Vec::new();
    let mut memory_candidates = Vec::new();

    for (step, m) in turn_messages.iter().enumerate() {
        if matches!(m.role, tiangong_types::MessageRole::Tool) {
            let text = extract_message_text(m);
            let is_error = m.tool_result_is_error;
            let tool_name = m.tool_name.clone().unwrap_or_default();
            let (file_path, url) = extract_paths_and_urls(&text);

            // 产物
            if !text.is_empty() {
                artifacts.push(TurnArtifact {
                    kind: if url.is_some() {
                        TurnArtifactKind::Media
                    } else if file_path.is_some() {
                        TurnArtifactKind::File
                    } else {
                        TurnArtifactKind::ToolResult
                    },
                    tool_name: Some(tool_name.clone()),
                    title: Some(
                        format!("{} {}", tool_name, if is_error { "失败" } else { "" })
                            .trim()
                            .to_string(),
                    ),
                    url: url.clone(),
                    path: file_path.clone(),
                    summary: Some(truncate(&text, 200)),
                });
            }

            // 候选
            memory_candidates.push(MemoryCandidate {
                tool_name: tool_name.clone(),
                step_index: step,
                hint: truncate(&text, 500),
                suggested_kinds: Vec::new(),
                file_path,
                url,
                result_summary: Some(truncate(&text, 200)),
                success: !is_error,
            });
        }

        // 从 Assistant 消息中提取媒体产物。
        if matches!(m.role, tiangong_types::MessageRole::Assistant) {
            for block in &m.content {
                if let tiangong_types::ContentBlock::Media { url, title, .. } = block {
                    artifacts.push(TurnArtifact {
                        kind: TurnArtifactKind::Media,
                        tool_name: None,
                        title: title.clone(),
                        url: Some(url.clone()),
                        path: None,
                        summary: None,
                    });
                }
            }
        }
    }

    let request = RunEnhancedMicroRuminationRequest {
        turn_result: EnhancedTurnResult {
            session_id: session.id,
            turn_id: format!("turn-{turn_start_idx}"),
            had_tool_calls: !tool_calls.is_empty(),
            user_input,
            summary,
            tool_calls,
            artifacts,
            workspace_id: state::workspace_id(),
            memory_candidates,
            turn_messages: turn_messages
                .iter()
                .map(|m| TurnMessage {
                    role: format!("{:?}", m.role).to_lowercase(),
                    content: extract_message_text(m),
                })
                .collect(),
        },
    };
    let _ = sidecar_client::invoke::<RunEnhancedMicroRumination>(&request);
    Ok(())
}

/// 从 session 快照提取 id/cwd，转发给 sidecar 做 meso 反刍。
fn forward_session_rumination(session_json: &str) -> Result<(), PluginError> {
    let session: tiangong_types::PluginSession = serde_json::from_str(session_json)
        .map_err(|e| PluginError::Message(format!("解析 PluginSession 失败: {e}")))?;

    let request = RunMesoRuminationRequest {
        session_id: session.id,
        // 使用统一的 workspace_id 生成逻辑，与 Micro 保持一致。
        workspace_id: workspace_id_from_cwd(&session.cwd),
    };
    let _ = sidecar_client::invoke::<RunMesoRumination>(&request);
    Ok(())
}

/// 从 Message 的 ContentBlock 列表中提取文本内容。
/// Message.content 是 Vec<ContentBlock>，需要遍历 Text 块拼接。
fn extract_message_text(message: &tiangong_types::Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            tiangong_types::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 截断字符串到指定字符数。
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect::<String>() + "…"
}

/// 从文本中提取文件路径和 URL。
fn extract_paths_and_urls(text: &str) -> (Option<String>, Option<String>) {
    let url = text
        .find("https://")
        .or_else(|| text.find("http://"))
        .map(|start| {
            let rest = &text[start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | ','))
                .unwrap_or(rest.len());
            rest[..end].to_string()
        });

    let path = text
        .find("/Users/")
        .or_else(|| text.find("/home/"))
        .or_else(|| text.find("/tmp/"))
        .or_else(|| text.find("./"))
        .map(|start| {
            let rest = &text[start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | ','))
                .unwrap_or(rest.len());
            rest[..end].to_string()
        });

    (path, url)
}

/// 统一的工作区 ID 生成：取 cwd 的末尾目录名。
fn workspace_id_from_cwd(cwd: &str) -> String {
    cwd.rsplit('/').next().unwrap_or(cwd).to_string()
}

/// WASM 内简易日志（无 tracing，直接忽略）。
fn tracing_warn(_msg: &str) {}

fn tool_result_ok(summary: String) -> ToolResult {
    ToolResult {
        ok: true,
        summary,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    }
}

bindings::export!(Component with_types_in bindings);
