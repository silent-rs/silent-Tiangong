//! Desktop 纯 TypeScript 插件工具调用桥接。
//!
//! 本模块只转发工具名、参数和结果，不识别任何具体插件或工具语义。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use tiangong_core::model::ToolCall;
use tiangong_core::tool::ToolResult;

const MAX_RESULT_FIELD_BYTES: usize = 2_000_000;

/// 每插件拉起请求冷却表：UI 挂载并完成订阅通常在秒级，冷却期内不重复
/// 请求，避免同一 turn 连续工具调用触发反复弹面板。
static UI_LAUNCH_LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
const UI_LAUNCH_COOLDOWN: Duration = Duration::from_secs(3);

/// 经 `app.open` 原语（mode=background 且不带实例编号）请求宿主挂载隐藏
/// 执行壳保证工具有人接应：Desktop 前端按「无编号的后台拉起」分流为
/// 隐藏挂载，不建可见标签；CLI / Server 未注入处理器时退化为等待超时。
/// 插件 UI 挂载完成订阅后由重放机制继续执行调用，工具随后携带精确实例
/// 编号再次 `app.open` 建立可见标签（是否展开面板由其 showPanel 决定）。
fn request_plugin_ui(plugin_id: &str, session_id: &str) {
    let cooldowns = UI_LAUNCH_LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut last_by_plugin) = cooldowns.lock() else {
        return;
    };
    let now = Instant::now();
    if last_by_plugin
        .get(plugin_id)
        .is_some_and(|last| now.duration_since(*last) < UI_LAUNCH_COOLDOWN)
    {
        return;
    }
    last_by_plugin.insert(plugin_id.to_string(), now);
    drop(last_by_plugin);
    let payload = serde_json::json!({ "session_id": session_id, "mode": "background" }).to_string();
    match crate::bridge::open_app_for_plugin(plugin_id, &payload) {
        Ok(_) => tracing::info!(
            plugin_id,
            session_id,
            "已请求后台挂载插件实例（app.open mode=background）"
        ),
        Err(error) => {
            tracing::warn!(%error, plugin_id, "请求后台挂载插件实例失败")
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TsToolInvocation {
    pub invocation_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TsToolResolution {
    pub invocation_id: String,
    #[serde(default)]
    pub status: TsToolCloseStatus,
    pub result: TsToolResult,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TsToolResult {
    pub ok: bool,
    pub summary: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TsToolCloseStatus {
    #[default]
    Answered,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct TsToolClosed {
    invocation_id: String,
    status: TsToolCloseStatus,
}

struct PendingCall {
    invocation: TsToolInvocation,
    plugin_id: String,
    deadline: NaiveDateTime,
    sender: oneshot::Sender<TsToolResolution>,
}

static PENDING: OnceLock<Mutex<HashMap<String, PendingCall>>> = OnceLock::new();

fn pending_calls() -> &'static Mutex<HashMap<String, PendingCall>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

struct PendingGuard {
    invocation_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let removed = pending_calls()
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.invocation_id));
        if let Some(call) = removed {
            emit_closed(
                &call.plugin_id,
                &self.invocation_id,
                TsToolCloseStatus::Cancelled,
            );
        }
    }
}

pub async fn execute(
    plugin_id: String,
    session_id: String,
    call: ToolCall,
    timeout_ms: u64,
) -> ToolResult {
    let invocation_id = scru128::new().to_string();
    let created_at = Local::now().naive_local();
    let deadline = created_at + chrono::Duration::milliseconds(timeout_ms as i64);
    let invocation = TsToolInvocation {
        invocation_id: invocation_id.clone(),
        session_id: session_id.clone(),
        tool_call_id: call.id,
        name: call.name,
        arguments: call.arguments,
        created_at: format_time(created_at),
    };
    let (sender, receiver) = oneshot::channel();
    pending_calls().lock().expect("TS 工具等待表锁损坏").insert(
        invocation_id.clone(),
        PendingCall {
            invocation: invocation.clone(),
            plugin_id: plugin_id.clone(),
            deadline,
            sender,
        },
    );
    let _guard = PendingGuard {
        invocation_id: invocation_id.clone(),
    };
    emit_requested(&plugin_id, &invocation);

    // 无人接应时请求宿主后台挂载插件实例（通用能力，不区分官方与三方
    // 插件）：实例挂载后 shell 订阅 tool.requested，bridge_subscribe 会
    // 重放本调用。
    let subscribed = crate::bridge::plugin_has_subscriber(&plugin_id, "tool.requested");
    tracing::info!(
        plugin_id = %plugin_id,
        invocation_id = %invocation_id,
        tool = %invocation.name,
        subscribed,
        "TS 工具等待接应（无订阅者时请求后台挂载插件实例）"
    );
    if !subscribed {
        // 既无界面可接应、又无 sidecar 可直连（直连在适配器层已分派，走到
        // 这里说明插件有页面路径）：立即明确失败，不傻等超时。
        let has_ui = crate::registry::plugin_manifest(&plugin_id).is_some_and(|manifest| {
            manifest
                .ui_contributions()
                .iter()
                .any(|contribution| contribution.slot == "extension.tab")
        });
        if !has_ui {
            let _ = pending_calls()
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&invocation_id));
            emit_closed(&plugin_id, &invocation_id, TsToolCloseStatus::Cancelled);
            return failure_result(format!(
                "插件 {plugin_id} 的工具 {} 无接应：插件未声明 extension.tab 界面，\
                 也没有可供直连的 sidecar；请补齐界面贡献或 sidecar 声明",
                invocation.name
            ));
        }
        request_plugin_ui(&plugin_id, &session_id);
    }

    match tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await {
        Ok(Ok(resolution)) => {
            let status = resolution.status;
            let result = resolution.result.into_tool_result();
            emit_closed(&plugin_id, &invocation_id, status);
            result
        }
        Ok(Err(_)) => failure_result("TypeScript 插件工具调用已取消"),
        Err(_) => {
            let removed = pending_calls()
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&invocation_id));
            if removed.is_some() {
                emit_closed(&plugin_id, &invocation_id, TsToolCloseStatus::Expired);
            }
            failure_result("TypeScript 插件工具调用超时")
        }
    }
}

pub fn resolve(plugin_id: &str, payload: &str) -> Result<String> {
    let resolution: TsToolResolution = serde_json::from_str(payload)
        .map_err(|error| anyhow::anyhow!("TS 工具结果格式无效：{error}"))?;
    validate_result(&resolution.result)?;

    let pending = {
        let mut calls = pending_calls()
            .lock()
            .map_err(|_| anyhow::anyhow!("TS 工具等待表已损坏"))?;
        let Some(call) = calls.get(&resolution.invocation_id) else {
            bail!("TS 工具调用不存在、已闭合或已过期");
        };
        if call.plugin_id != plugin_id {
            bail!("TS 工具调用不属于插件 {plugin_id}");
        }
        if Local::now().naive_local() >= call.deadline {
            let expired = calls.remove(&resolution.invocation_id);
            drop(calls);
            if let Some(expired) = expired {
                emit_closed(
                    &expired.plugin_id,
                    &resolution.invocation_id,
                    TsToolCloseStatus::Expired,
                );
            }
            bail!("TS 工具调用已过期");
        }
        calls
            .remove(&resolution.invocation_id)
            .expect("已确认存在的 TS 工具调用必须可移除")
    };

    if pending.sender.send(resolution).is_err() {
        emit_closed(
            &pending.plugin_id,
            &pending.invocation.invocation_id,
            TsToolCloseStatus::Cancelled,
        );
        bail!("TS 工具调用等待端已关闭");
    }
    Ok("true".to_string())
}

pub fn replay_pending(plugin_id: &str, channel: &str) {
    if channel != "tool.requested" {
        return;
    }
    let mut invocations = pending_calls()
        .lock()
        .map(|pending| {
            pending
                .values()
                .filter(|call| call.plugin_id == plugin_id)
                .map(|call| call.invocation.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    invocations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.invocation_id.cmp(&right.invocation_id))
    });
    for invocation in invocations {
        emit_requested(plugin_id, &invocation);
    }
}

pub fn cancel_plugin_calls(plugin_id: &str) {
    let removed = pending_calls()
        .lock()
        .map(|mut pending| {
            let ids = pending
                .iter()
                .filter(|(_, call)| call.plugin_id == plugin_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|call| (id, call)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (invocation_id, call) in removed {
        emit_closed(
            &call.plugin_id,
            &invocation_id,
            TsToolCloseStatus::Cancelled,
        );
    }
}

pub fn cancel_all_calls() {
    let plugin_ids = pending_calls()
        .lock()
        .map(|pending| {
            pending
                .values()
                .map(|call| call.plugin_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    for plugin_id in plugin_ids {
        cancel_plugin_calls(&plugin_id);
    }
}

fn validate_result(result: &TsToolResult) -> Result<()> {
    for (name, value) in [
        ("summary", &result.summary),
        ("stdout", &result.stdout),
        ("stderr", &result.stderr),
    ] {
        if value.len() > MAX_RESULT_FIELD_BYTES {
            bail!("TS 工具结果字段 {name} 超过 2MB");
        }
    }
    Ok(())
}

fn emit_requested(plugin_id: &str, invocation: &TsToolInvocation) {
    if let Ok(payload) = serde_json::to_string(invocation) {
        crate::bridge::bridge_emit_to(plugin_id, "tool.requested", &payload);
    } else {
        tracing::warn!(plugin_id, "序列化 TS 工具调用事件失败");
    }
}

fn emit_closed(plugin_id: &str, invocation_id: &str, status: TsToolCloseStatus) {
    let event = TsToolClosed {
        invocation_id: invocation_id.to_string(),
        status,
    };
    if let Ok(payload) = serde_json::to_string(&event) {
        crate::bridge::bridge_emit_to(plugin_id, "tool.closed", &payload);
    } else {
        tracing::warn!(plugin_id, invocation_id, "序列化 TS 工具闭合事件失败");
    }
}

fn failure_result(message: impl Into<String>) -> ToolResult {
    let message = message.into();
    ToolResult {
        ok: false,
        summary: message.clone(),
        stdout: String::new(),
        stderr: message,
        exit_code: 1,
        execution: None,
    }
}

fn format_time(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

impl TsToolResult {
    fn into_tool_result(self) -> ToolResult {
        ToolResult {
            ok: self.ok,
            summary: self.summary,
            stdout: self.stdout,
            stderr: self.stderr,
            exit_code: self.exit_code,
            execution: None,
        }
    }
}
