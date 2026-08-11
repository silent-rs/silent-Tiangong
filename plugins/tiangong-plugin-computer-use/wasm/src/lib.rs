//! Computer Use 插件的 WASM 桥接组件。
//!
//! 本组件只做桥接：声明六个桌面控制工具、解析参数、注入提示词与生命周期入口；
//! 真实无障碍访问经 `sidecar.invoke` 转发到 sidecar 进程（Windows UI Automation、
//! macOS AXUIElement、Linux AT-SPI2 由 sidecar 按平台实现）。wasm 侧不做任何
//! 系统调用。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use serde::Serialize;
use serde_json::json;
use tiangong_plugin_computer_use_protocol::ops::{
    Action, ActionRequest, ActionRequestKind, DesktopStatus, DesktopStatusRequest, Find,
    FindConditions, FindRequest, ListWindows, ListWindowsRequest, SetAccess, SetAccessRequest,
    Snapshot, SnapshotRequest, Wait, WaitRequest,
};
use tiangong_plugin_computer_use_protocol::{
    ComputerUseOperation, DesktopResult, ElementRef, MatchMode, TOOL_DESKTOP_ACTION,
    TOOL_DESKTOP_FIND, TOOL_DESKTOP_LIST_WINDOWS, TOOL_DESKTOP_SNAPSHOT, TOOL_DESKTOP_STATUS,
    TOOL_DESKTOP_WAIT,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_computer_use_protocol::PLUGIN_ID;
    pub const NAME: &str = "Computer Use";
    pub const VERSION: &str = tiangong_plugin_computer_use_protocol::PLUGIN_VERSION;
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        full_trust: bool,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState { full_trust: false }) };
    }

    pub fn set_full_trust(full_trust: bool) {
        STATE.with(|s| s.borrow_mut().full_trust = full_trust);
    }

    pub fn access_context() -> tiangong_plugin_computer_use_protocol::AccessContext {
        STATE.with(|s| tiangong_plugin_computer_use_protocol::AccessContext {
            full_trust: s.borrow().full_trust,
        })
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
                name: TOOL_DESKTOP_STATUS.to_string(),
                description: "查询当前平台、图形会话、无障碍能力与受支持动作，不触发任何应用动作。"
                    .to_string(),
                input_schema: schema_string(json!({ "type": "object", "properties": {} })),
            },
            ToolSpec {
                name: TOOL_DESKTOP_LIST_WINDOWS.to_string(),
                description: "列出当前可访问的应用和顶层窗口，支持按应用名、进程号和是否前台筛选。"
                    .to_string(),
                input_schema: schema_string(json!({
                    "type": "object",
                    "properties": {
                        "app_name": { "type": "string", "description": "按应用名称筛选（包含匹配）" },
                        "pid": { "type": "integer", "description": "按进程编号筛选", "minimum": 0 },
                        "foreground_only": { "type": "boolean", "description": "仅返回前台窗口" }
                    }
                })),
            },
            ToolSpec {
                name: TOOL_DESKTOP_SNAPSHOT.to_string(),
                description: "读取指定应用或窗口的控件树，支持限制最大深度、节点数及是否包含不可见控件。"
                    .to_string(),
                input_schema: schema_string(json!({
                    "type": "object",
                    "properties": {
                        "window": element_ref_schema(),
                        "app_name": { "type": "string", "description": "按应用名称定位窗口（window 未提供时使用）" },
                        "pid": { "type": "integer", "description": "按进程编号定位窗口", "minimum": 0 },
                        "max_depth": { "type": "integer", "description": "最大遍历深度，0 使用默认", "minimum": 0 },
                        "max_nodes": { "type": "integer", "description": "最大节点数，0 使用默认", "minimum": 0 },
                        "include_invisible": { "type": "boolean", "description": "是否包含不可见控件" }
                    }
                })),
            },
            ToolSpec {
                name: TOOL_DESKTOP_FIND.to_string(),
                description: "在窗口或快照内按稳定标识、类型、名称、值等组合查找控件；多匹配时返回候选不默认操作。"
                    .to_string(),
                input_schema: schema_string(json!({
                    "type": "object",
                    "properties": {
                        "window": element_ref_schema(),
                        "snapshot": { "type": "integer", "description": "在指定快照版本内查找", "minimum": 0 },
                        "conditions": {
                            "type": "object",
                            "properties": {
                                "automation_id": { "type": "string" },
                                "role": { "type": "string" },
                                "name": { "type": "string" },
                                "value": { "type": "string" },
                                "visible": { "type": "boolean" },
                                "enabled": { "type": "boolean" },
                                "focused": { "type": "boolean" },
                                "mode": { "type": "string", "enum": ["exact", "contains"], "default": "exact" }
                            }
                        },
                        "max_candidates": { "type": "integer", "description": "最大返回候选数，0 使用默认", "minimum": 0 }
                    },
                    "required": ["conditions"]
                })),
            },
            ToolSpec {
                name: TOOL_DESKTOP_ACTION.to_string(),
                description: "对明确的临时控件引用执行结构化动作（focus/press/set_value/toggle/select/expand/collapse/scroll_into_view）。"
                    .to_string(),
                input_schema: schema_string(json!({
                    "type": "object",
                    "properties": {
                        "element": element_ref_schema(),
                        "action": {
                            "type": "string",
                            "enum": ["focus", "press", "set_value", "toggle", "select", "expand", "collapse", "scroll_into_view"]
                        },
                        "value": { "type": "string", "description": "set_value 的值" },
                        "selection": { "type": "string", "description": "select 的选项标识" }
                    },
                    "required": ["element", "action"]
                })),
            },
            ToolSpec {
                name: TOOL_DESKTOP_WAIT.to_string(),
                description: "等待窗口或控件满足出现、消失、获得焦点、可用状态变化或值变化，有明确超时。"
                    .to_string(),
                input_schema: schema_string(json!({
                    "type": "object",
                    "properties": {
                        "condition": {
                            "type": "object",
                            "description": "等待条件，kind 为 appear/disappear/focus/available/value 之一"
                        },
                        "timeout_ms": { "type": "integer", "description": "超时毫秒数，必须大于 0", "minimum": 1 }
                    },
                    "required": ["condition", "timeout_ms"]
                })),
            },
        ])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(vec![
            "桌面应用控制优先使用 computer-use 插件：先 desktop_status 确认能力，再 desktop_list_windows 定位窗口，desktop_snapshot 读取控件树，desktop_find 精确匹配控件后用 desktop_action 执行动作，动作后用 desktop_wait 确认状态。网页内容继续优先交给浏览器插件。".to_string(),
            "桌面控件以语义定位为主：优先用稳定标识（automation_id）与控件类型（role）匹配，名称仅作补充；同名控件返回多个候选时不得默认操作第一个，需进一步限定。控件引用只在本次快照内有效，动作前必须重新确认目标。".to_string(),
        ])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_DESKTOP_STATUS => handle_desktop_status(call.arguments),
            TOOL_DESKTOP_LIST_WINDOWS => handle_list_windows(call.arguments),
            TOOL_DESKTOP_SNAPSHOT => handle_snapshot(call.arguments),
            TOOL_DESKTOP_FIND => handle_find(call.arguments),
            TOOL_DESKTOP_ACTION => handle_action(call.arguments),
            TOOL_DESKTOP_WAIT => handle_wait(call.arguments),
            other => Err(plugin_err(format!("未知的 Computer Use 工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(_workspace: Option<String>, full_trust: bool) -> Result<(), PluginError> {
        state::set_full_trust(full_trust);
        let request = SetAccessRequest { full_trust };
        sidecar_client::invoke::<SetAccess>(&request)
            .map_err(|error| plugin_err(format!("set_access 调用 sidecar 失败: {error}")))?;
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ready(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_finished(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }
}

// ── 工具处理 ───────────────────────────────────────────────────

fn handle_desktop_status(arguments: String) -> Result<ToolResult, PluginError> {
    // desktop_status 无必填参数，但仍校验 JSON 合法性，非法输入返回错误。
    if let Err(f) = parse_args("desktop_status", &arguments) {
        return Ok(f);
    }
    let request = DesktopStatusRequest {
        access: state::access_context(),
    };
    run_desktop_op::<DesktopStatus, _>(&request, "desktop_status")
}

fn handle_list_windows(arguments: String) -> Result<ToolResult, PluginError> {
    let args = match parse_args("desktop_list_windows", &arguments) {
        Ok(v) => v,
        Err(f) => return Ok(f),
    };
    let pid = args.get("pid").and_then(as_u32_bounded);
    // pid 超范围（超 u32）时返回参数错误，而非截断成另一个有效值。
    if args.get("pid").is_some() && pid.is_none() {
        return Ok(tool_failure(
            "desktop_list_windows 的 pid 超出有效范围",
            "pid out of range",
        ));
    }
    let request = ListWindowsRequest {
        app_name: args.get("app_name").and_then(as_str_owned),
        pid,
        foreground_only: args
            .get("foreground_only")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        access: state::access_context(),
    };
    run_desktop_op::<ListWindows, _>(&request, "desktop_list_windows")
}

fn handle_snapshot(arguments: String) -> Result<ToolResult, PluginError> {
    let args = match parse_args("desktop_snapshot", &arguments) {
        Ok(v) => v,
        Err(f) => return Ok(f),
    };
    // pid 超范围返回参数错误，而非截断。
    let pid = args.get("pid").and_then(as_u32_bounded);
    if args.get("pid").is_some() && pid.is_none() {
        return Ok(tool_failure(
            "desktop_snapshot 的 pid 超出有效范围",
            "pid out of range",
        ));
    }
    // max_depth/max_nodes 超范围返回参数错误，而非截断。
    let max_depth = match args.get("max_depth") {
        Some(v) if !v.is_null() => match as_u32_bounded(v) {
            Some(d) => d,
            None => {
                return Ok(tool_failure(
                    "desktop_snapshot 的 max_depth 超出有效范围",
                    "max_depth out of range",
                ));
            }
        },
        _ => 0,
    };
    let max_nodes = match args.get("max_nodes") {
        Some(v) if !v.is_null() => match as_u32_bounded(v) {
            Some(n) => n,
            None => {
                return Ok(tool_failure(
                    "desktop_snapshot 的 max_nodes 超出有效范围",
                    "max_nodes out of range",
                ));
            }
        },
        _ => 0,
    };
    let request = SnapshotRequest {
        scope: tiangong_plugin_computer_use_protocol::ops::SnapshotScope {
            window: args.get("window").and_then(parse_element_ref),
            app_name: args.get("app_name").and_then(as_str_owned),
            pid,
        },
        max_depth,
        max_nodes,
        include_invisible: args
            .get("include_invisible")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        access: state::access_context(),
    };
    run_desktop_op::<Snapshot, _>(&request, "desktop_snapshot")
}

fn handle_find(arguments: String) -> Result<ToolResult, PluginError> {
    let args = match parse_args("desktop_find", &arguments) {
        Ok(v) => v,
        Err(f) => return Ok(f),
    };
    // conditions 缺失时返回参数错误（它是必填字段）。
    let conditions_raw = match args.get("conditions") {
        Some(c) if !c.is_null() => c.clone(),
        _ => {
            return Ok(tool_failure(
                "desktop_find 缺少必填的 conditions 参数",
                "missing conditions",
            ));
        }
    };
    let request = FindRequest {
        window: args.get("window").and_then(parse_element_ref),
        snapshot: args.get("snapshot").and_then(serde_json::Value::as_u64),
        conditions: FindConditions {
            automation_id: conditions_raw.get("automation_id").and_then(as_str_owned),
            role: conditions_raw.get("role").and_then(as_str_owned),
            name: conditions_raw.get("name").and_then(as_str_owned),
            value: conditions_raw.get("value").and_then(as_str_owned),
            visible: conditions_raw
                .get("visible")
                .and_then(serde_json::Value::as_bool),
            enabled: conditions_raw
                .get("enabled")
                .and_then(serde_json::Value::as_bool),
            focused: conditions_raw
                .get("focused")
                .and_then(serde_json::Value::as_bool),
            mode: conditions_raw
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .map(|s| match s {
                    "contains" => MatchMode::Contains,
                    _ => MatchMode::Exact,
                })
                .unwrap_or_default(),
        },
        // max_candidates 超范围返回参数错误，而非截断（与 pid/depth/nodes 一致）。
        max_candidates: match args.get("max_candidates") {
            Some(v) if !v.is_null() => match as_u32_bounded(v) {
                Some(n) => n,
                None => {
                    return Ok(tool_failure(
                        "desktop_find 的 max_candidates 超出有效范围",
                        "max_candidates out of range",
                    ));
                }
            },
            _ => 0,
        },
        access: state::access_context(),
    };
    run_desktop_op::<Find, _>(&request, "desktop_find")
}

fn handle_action(arguments: String) -> Result<ToolResult, PluginError> {
    let args = match parse_args("desktop_action", &arguments) {
        Ok(v) => v,
        Err(f) => return Ok(f),
    };
    let element = match args.get("element").and_then(parse_element_ref) {
        Some(e) => e,
        None => {
            return Ok(tool_failure(
                "desktop_action 缺少 element 参数",
                "missing element",
            ));
        }
    };
    let action_str = args
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let action = match parse_action_kind(action_str) {
        Some(a) => a,
        None => {
            return Ok(tool_failure(
                &format!("desktop_action 不支持的动作: {action_str}"),
                "unsupported action",
            ));
        }
    };
    let request = ActionRequest {
        element,
        action,
        value: args.get("value").and_then(as_str_owned),
        selection: args.get("selection").and_then(as_str_owned),
        access: state::access_context(),
    };
    run_desktop_op::<Action, _>(&request, "desktop_action")
}

fn handle_wait(arguments: String) -> Result<ToolResult, PluginError> {
    let args = match parse_args("desktop_wait", &arguments) {
        Ok(v) => v,
        Err(f) => return Ok(f),
    };
    let condition = match parse_wait_condition(&args) {
        Some(c) => c,
        None => {
            return Ok(tool_failure(
                "desktop_wait 缺少或无法解析 condition",
                "bad condition",
            ));
        }
    };
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if timeout_ms == 0 {
        return Ok(tool_failure(
            "desktop_wait 的 timeout_ms 必须大于 0",
            "bad timeout",
        ));
    }
    // 限制等待不超过 host 请求期限（plugin.json 的 request_timeout_ms=60000）。
    // 超过时截断到 55 秒（留 5 秒余量），避免 host 在 60 秒断开后 sidecar 仍悬挂。
    const MAX_WAIT_MS: u64 = 55_000;
    let timeout_ms = timeout_ms.min(MAX_WAIT_MS);
    let request = WaitRequest {
        condition,
        timeout_ms,
        access: state::access_context(),
    };
    run_desktop_op::<Wait, _>(&request, "desktop_wait")
}

/// 统一执行一个桌面操作：调 sidecar，把 DesktopResult<T> 转 ToolResult。
fn run_desktop_op<O, T>(request: &O::Request, tool: &str) -> Result<ToolResult, PluginError>
where
    O: ComputerUseOperation<Response = DesktopResult<T>>,
    O::Request: Serialize,
    T: Serialize,
{
    let result = sidecar_client::invoke::<O>(request)
        .map_err(|e| plugin_err(format!("{tool} 调用 sidecar 失败: {e}")))?;
    Ok(desktop_result_to_tool_result(result))
}

/// DesktopResult<T> → ToolResult：成功序列化进 stdout，业务错误用 agent_message 进 stderr。
fn desktop_result_to_tool_result<T: Serialize>(result: DesktopResult<T>) -> ToolResult {
    match result {
        DesktopResult::Ok(value) => {
            let stdout = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
            ToolResult {
                ok: true,
                summary: "桌面操作完成".to_string(),
                stdout,
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            }
        }
        DesktopResult::Err(error) => {
            let message = error.agent_message();
            ToolResult {
                ok: false,
                summary: message.clone(),
                stdout: String::new(),
                stderr: message,
                exit_code: 1,
                execution: None,
            }
        }
    }
}

// ── 参数解析辅助 ───────────────────────────────────────────────

/// 解析工具参数 JSON。非法 JSON 返回参数错误 ToolResult，而非当作空对象。
#[allow(clippy::result_large_err)]
fn parse_args(tool: &str, arguments: &str) -> Result<serde_json::Value, ToolResult> {
    serde_json::from_str(arguments).map_err(|e| {
        tool_failure(
            &format!("{tool} 参数不是合法 JSON: {e}"),
            "invalid json arguments",
        )
    })
}

/// 把 JSON 数值校验后转为 u32，超范围返回 None（调用方决定如何处理）。
fn as_u32_bounded(v: &serde_json::Value) -> Option<u32> {
    let n = v.as_u64()?;
    if n <= u32::MAX as u64 {
        Some(n as u32)
    } else {
        None
    }
}

/// 把 JSON 值的字符串视图转为 owned String。
fn as_str_owned(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(String::from)
}

fn parse_element_ref(v: &serde_json::Value) -> Option<ElementRef> {
    let obj = v.as_object()?;
    let id = obj.get("id")?.as_str()?.to_string();
    let snapshot = obj.get("snapshot")?.as_u64()?;
    Some(ElementRef { id, snapshot })
}

fn element_ref_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "snapshot": { "type": "integer", "minimum": 0 }
        },
        "required": ["id", "snapshot"]
    })
}

fn parse_action_kind(s: &str) -> Option<ActionRequestKind> {
    Some(match s {
        "focus" => ActionRequestKind::Focus,
        "press" => ActionRequestKind::Press,
        "set_value" => ActionRequestKind::SetValue,
        "toggle" => ActionRequestKind::Toggle,
        "select" => ActionRequestKind::Select,
        "expand" => ActionRequestKind::Expand,
        "collapse" => ActionRequestKind::Collapse,
        "scroll_into_view" => ActionRequestKind::ScrollIntoView,
        _ => return None,
    })
}

fn parse_wait_target(
    v: &serde_json::Value,
) -> Option<tiangong_plugin_computer_use_protocol::ops::WaitTarget> {
    Some(tiangong_plugin_computer_use_protocol::ops::WaitTarget {
        app_name: v.get("app_name").and_then(as_str_owned),
        title: v.get("title").and_then(as_str_owned),
    })
}

fn parse_wait_condition(
    args: &serde_json::Value,
) -> Option<tiangong_plugin_computer_use_protocol::ops::WaitCondition> {
    use tiangong_plugin_computer_use_protocol::ops::WaitCondition;
    let cond = args.get("condition")?;
    let kind = cond.get("kind")?.as_str()?;
    Some(match kind {
        "appear" | "disappear" => {
            let target = parse_wait_target(cond.get("target")?)?;
            // appear/disappear 的目标必须至少提供 app_name 或 title，
            // 否则 appear 会一直等到超时、disappear 会立即误判成功。
            if target.app_name.is_none() && target.title.is_none() {
                return None;
            }
            if kind == "appear" {
                WaitCondition::Appear { target }
            } else {
                WaitCondition::Disappear { target }
            }
        }
        "focus" => WaitCondition::Focus {
            element: parse_element_ref(cond.get("element")?)?,
        },
        "available" => WaitCondition::Available {
            element: parse_element_ref(cond.get("element")?)?,
        },
        "value" => WaitCondition::Value {
            element: parse_element_ref(cond.get("element")?)?,
            expected: cond.get("expected").and_then(as_str_owned),
        },
        _ => return None,
    })
}

fn schema_string(value: serde_json::Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())
}

/// 构造简单失败 ToolResult。
fn tool_failure(summary: &str, stderr: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: summary.to_string(),
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: 1,
        execution: None,
    }
}

/// Computer Use 插件无设置页：contributions 返回空，其余入口报错。
impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Computer Use 插件暂无设置页面"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Computer Use 插件暂无页面资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Computer Use 插件暂无页面消息"))
    }
}

bindings::export!(Component with_types_in bindings);
