//! Index 插件的 WASM 桥接组件。
//!
//! 本组件只做桥接：工具规格/工具执行/生命周期钩子全部转发到 Index sidecar。
//! 重型原生依赖（tantivy 索引、rg/grep 子进程、后台扫描）全部在 sidecar 进程内运行，
//! WASM 沙箱仅负责参数解析、session 快照提取与 IPC 转发。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use tiangong_plugin_index_protocol::lifecycle::{
    FinalizeSession, FinalizeSessionRequest, IndexTurnBatch, IndexTurnBatchRequest, SetWorkspace,
    SetWorkspaceRequest,
};
use tiangong_plugin_index_protocol::management::{
    DeleteWorkspaceIndex, DeleteWorkspaceIndexRequest, ListWorkspaceIndexes, RebuildWorkspaceIndex,
    RebuildWorkspaceIndexRequest,
};
use tiangong_plugin_index_protocol::search::{
    IndexSearch, IndexSearchRequest, IndexSearchResponse, SearchCode, SearchCodeRequest,
    SearchCodeResponse,
};
use tiangong_plugin_index_protocol::{IndexOperation, IndexScope, TurnData};
use tiangong_types::{MessageRole, PluginSession};

mod descriptor {
    pub const ID: &str = tiangong_plugin_index_protocol::PLUGIN_ID;
    pub const NAME: &str = "Index";
    pub const VERSION: &str = tiangong_plugin_index_protocol::PLUGIN_VERSION;
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        workspace: Option<String>,
        session_id: Option<String>,
        full_trust: bool,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState {
            workspace: None,
            session_id: None,
            full_trust: false,
        }) };
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn set_full_trust(full_trust: bool) {
        STATE.with(|s| s.borrow_mut().full_trust = full_trust);
    }

    pub fn full_trust() -> bool {
        STATE.with(|s| s.borrow().full_trust)
    }

    pub fn workspace() -> Option<String> {
        STATE.with(|s| s.borrow().workspace.clone())
    }

    pub fn set_session_id(id: Option<String>) {
        STATE.with(|s| s.borrow_mut().session_id = id);
    }

    pub fn session_id() -> Option<String> {
        STATE.with(|s| s.borrow().session_id.clone())
    }
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

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
        Ok(vec![
            ToolSpec {
                name: "index_search".to_string(),
                description: "搜索当前工作区的文件内容和对话历史索引。查找代码文件、符号定义或之前的对话内容时优先使用此工具，需要精确定位代码行时配合 search_code 使用。仅在索引结果不足时再使用 recall_memory。".to_string(),
                input_schema: serde_json::to_string(&serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索关键词，支持文件路径、代码片段、符号名、对话内容"
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["workspace", "session", "all"],
                            "description": "搜索范围：workspace=仅文件索引，session=仅对话索引，all=全部（默认 all）"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "最多返回多少条结果，默认 10，最大 20",
                            "minimum": 1,
                            "maximum": 20
                        }
                    },
                    "required": ["query"]
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            },
            ToolSpec {
                name: "search_code".to_string(),
                description: "在目录中精确检索文本（优先使用 ripgrep/rg，rg 缺失时回退到 grep 较慢；需精确定位代码行时与 index_search 配合）".to_string(),
                input_schema: serde_json::to_string(&serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "检索文本或正则模式" },
                        "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录。非完全信任模式下限制在工作区内；完全信任模式下可读取工作区外路径" }
                    },
                    "required": ["pattern"]
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            },
        ])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        // 注入检索工具使用指引：说明 search_code 与 index_search 的配合用法。
        Ok(vec![
            "## 检索工具使用指引\n\
             - search_code 用于精确文本/正则检索，优先使用 rg；若环境缺失 rg，工具会自动\
             回退到 grep，可能较慢。调用 search_code 时应尽量指定更小的 path 和更精确的\
             pattern，避免全仓搜索导致超时。\n\
             - index_search 用于基于索引的语义检索（工作区文件 + 对话历史），速度更快但\
             受索引覆盖范围限制；需要精确定位某行代码时优先用 index_search 缩小范围，\
             再用 search_code 取精确行号。"
                .to_string(),
        ])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            "index_search" => handle_index_search(&call),
            "search_code" => handle_search_code(&call),
            other => Err(plugin_err(format!("未知的 Index 工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>, full_trust: bool) -> Result<(), PluginError> {
        state::set_workspace(workspace.clone());
        state::set_full_trust(full_trust);
        // 通知 sidecar 工作区变更并触发后台扫描。
        let request = SetWorkspaceRequest { workspace };
        sidecar_client::invoke::<SetWorkspace>(&request)
            .map_err(|error| plugin_err(format!("set_workspace 调用 sidecar 失败: {error}")))?;
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ready(session_json: String) -> Result<(), PluginError> {
        // 缓存 session_id 供 index_search 的 session 范围使用。
        if let Ok(session) = serde_json::from_str::<PluginSession>(&session_json) {
            state::set_session_id(Some(session.id));
        }
        Ok(())
    }

    fn on_turn_started(session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        if let Ok(session) = serde_json::from_str::<PluginSession>(&session_json) {
            state::set_session_id(Some(session.id));
        }
        Ok(())
    }

    fn on_turn_finished(session_json: String, turn_start_idx: u32) -> Result<(), PluginError> {
        forward_turn_batch(&session_json, turn_start_idx)
    }

    fn on_session_ended(session_json: String) -> Result<(), PluginError> {
        forward_finalize(&session_json)
    }
}

/// 处理 index_search 工具：转发到 sidecar 并把响应组装成 ToolResult。
fn handle_index_search(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
    let query = args
        .get("query")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Ok(tool_failure(
            false,
            "查询为空",
            "query parameter is required",
        ));
    }
    let scope = match args.get("scope").and_then(serde_json::Value::as_str) {
        Some("workspace") => IndexScope::Workspace,
        Some("session") => IndexScope::Session,
        _ => IndexScope::All,
    };
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 20) as usize;

    let request = IndexSearchRequest {
        query,
        scope,
        limit,
        workspace: state::workspace(),
        session_id: state::session_id(),
    };
    let response: IndexSearchResponse = sidecar_client::invoke::<IndexSearch>(&request)
        .map_err(|e| plugin_err(format!("index_search 执行失败: {e}")))?;

    let mut stdout_parts: Vec<String> = Vec::new();
    // 后台扫描进行中时 workspace 部分降级提示。
    if response.scanning {
        stdout_parts.push("【工作区索引正在构建中，请稍候】".to_string());
    } else if !response.workspace_hits.is_empty() {
        stdout_parts.push("【工作区文件】".to_string());
        for hit in &response.workspace_hits {
            stdout_parts.push(format!("- {} ({})", hit.path, hit.language));
        }
    }
    if !response.session_hits.is_empty() {
        stdout_parts.push("【对话历史】".to_string());
        for hit in &response.session_hits {
            let preview: String = hit.content.chars().take(200).collect();
            stdout_parts.push(format!("- [{}] {}", hit.role, preview));
        }
    }

    if stdout_parts.is_empty() {
        Ok(tool_failure(
            true,
            &format!("未找到与 \"{}\" 相关的索引结果", request.query),
            "",
        ))
    } else {
        let stdout = stdout_parts.join("\n");
        let count = stdout_parts.iter().filter(|l| l.starts_with('-')).count();
        Ok(ToolResult {
            ok: true,
            summary: format!("找到 {count} 条索引结果"),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

/// 处理 search_code 工具：转发到 sidecar（sidecar 内 spawn rg/grep 子进程）。
fn handle_search_code(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
    let pattern = args
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if pattern.is_empty() {
        return Ok(tool_failure(
            false,
            "search_code 缺少 pattern 参数",
            "empty pattern",
        ));
    }
    let request = SearchCodeRequest {
        pattern,
        path: args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        workspace: state::workspace(),
        // 信任模式由 host 在 set_workspace 时注入；sidecar 据此决定 search_code
        // 的路径校验严格度（full_trust 时放宽工作区外路径）。
        full_trust: state::full_trust(),
    };
    let response: SearchCodeResponse = sidecar_client::invoke::<SearchCode>(&request)
        .map_err(|e| plugin_err(format!("search_code 执行失败: {e}")))?;
    Ok(ToolResult {
        ok: response.ok,
        summary: response.summary,
        stdout: response.stdout,
        stderr: response.stderr,
        exit_code: response.exit_code as i32,
        execution: None,
    })
}

/// 从 PluginSession 提取本轮消息，组装成 TurnData 批量转发给 sidecar。
fn forward_turn_batch(session_json: &str, turn_start_idx: u32) -> Result<(), PluginError> {
    let session: PluginSession = serde_json::from_str(session_json)
        .map_err(|e| plugin_err(format!("解析 session 失败: {e}")))?;
    let start = turn_start_idx as usize;
    let turns: Vec<TurnData> = session
        .messages
        .get(start..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|msg| {
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                MessageRole::System => return None,
            };
            Some(TurnData {
                turn_id: msg.id.clone(),
                workspace_id: session.cwd.clone(),
                role: role.to_string(),
                content: msg.text_content(),
                topics: Vec::new(),
                entity_names: Vec::new(),
            })
        })
        .collect();
    if turns.is_empty() {
        return Ok(());
    }
    let request = IndexTurnBatchRequest {
        session_id: session.id,
        turns,
    };
    sidecar_client::invoke::<IndexTurnBatch>(&request)
        .map_err(|error| plugin_err(format!("on_turn_finished 调用 sidecar 失败: {error}")))?;
    Ok(())
}

/// on_session_ended：通知 sidecar finalize 会话索引。
fn forward_finalize(session_json: &str) -> Result<(), PluginError> {
    let session: PluginSession = serde_json::from_str(session_json)
        .map_err(|e| plugin_err(format!("解析 session 失败: {e}")))?;
    let request = FinalizeSessionRequest {
        session_id: session.id,
    };
    sidecar_client::invoke::<FinalizeSession>(&request)
        .map_err(|error| plugin_err(format!("on_session_ended 调用 sidecar 失败: {error}")))?;
    Ok(())
}

/// 构造简单 ToolResult（index_search 内部使用）。
fn tool_failure(ok: bool, summary: &str, stderr: &str) -> ToolResult {
    ToolResult {
        ok,
        summary: summary.to_string(),
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: if ok { 0 } else { 1 },
        execution: None,
    }
}

/// 索引管理设置页模板（单文件内联，与 memory 设置页同构）。
const INDEX_PAGE_TEMPLATE: &str = include_str!("index.html");
const INDEX_PAGE_CSS: &str = include_str!("index.css");
const INDEX_PAGE_JS: &str = include_str!("index.js");

/// 把 CSS/JS 内联进 HTML 占位符，返回完整设置页 HTML。
fn index_settings_html() -> String {
    INDEX_PAGE_TEMPLATE
        .replace("/*__INDEX_CSS__*/", INDEX_PAGE_CSS)
        .replace("/*__INDEX_JS__*/", INDEX_PAGE_JS)
}

/// 索引管理设置页 UiGuest 实现：列出工作区索引 + 重建/删除。
///
/// 与 memory 设置页同构——contributions 声明设置页贡献，open_view 返回内联 HTML，
/// handle_view_message 按 method 转发到 sidecar（list/delete/rebuild）。
impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(vec![Contribution {
            id: "index-settings".to_string(),
            title: "索引管理".to_string(),
            description: "工作区索引列表与重建/删除".to_string(),
            icon: "database".to_string(),
            group: "plugins".to_string(),
            has_view: true,
        }])
    }

    fn open_view(contribution_id: String) -> Result<ViewResponse, PluginError> {
        if contribution_id != "index-settings" {
            return Err(plugin_err(format!(
                "未知的 contribution: {contribution_id}"
            )));
        }
        Ok(ViewResponse {
            html: index_settings_html(),
        })
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Index 设置页无外部资源"))
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "list" => {
                invoke_for_ui::<ListWorkspaceIndexes>(&tiangong_plugin_index_protocol::Empty {})?
            }
            "delete" => {
                let req: DeleteWorkspaceIndexRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析删除请求失败: {e}")))?;
                invoke_for_ui::<DeleteWorkspaceIndex>(&req)?
            }
            "rebuild" => {
                let req: RebuildWorkspaceIndexRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析重建请求失败: {e}")))?;
                invoke_for_ui::<RebuildWorkspaceIndex>(&req)?
            }
            other => return Err(plugin_err(format!("未知的索引管理消息: {other}"))),
        };
        Ok(ViewMessageResponse { payload })
    }
}

/// 通用 sidecar 转发器：调用操作 O 并把响应序列化成 JSON 字符串（供 iframe 消费）。
fn invoke_for_ui<O>(request: &O::Request) -> Result<String, PluginError>
where
    O: IndexOperation,
    O::Response: serde::Serialize,
{
    let response = sidecar_client::invoke::<O>(request).map_err(|e| plugin_err(e.to_string()))?;
    serde_json::to_string(&response).map_err(|e| plugin_err(e.to_string()))
}

bindings::export!(Component with_types_in bindings);
