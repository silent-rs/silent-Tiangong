//! Sub Agent 生命周期管理与 7 个团队工具 handler（迁自 core
//! `agent_team/lifecycle.rs`）。
//!
//! 本模块承接原 core 的 `execute_create_agent` / `execute_dismiss_agent` /
//! `execute_send_message` / `execute_broadcast_message` / `execute_notify_user` /
//! `execute_lock_file` / `execute_unlock_file`，以及 `restore_agents_from_session_history`
//! / `persist_child_session` / `parse_agent_mention_route` 等辅助逻辑。
//!
//! 与原 core 实现的差异：
//! - 所有 handler 的 `team` 入参改为本插件的 [`crate::TeamContext`]（字段同构）。
//! - import 路径 `crate::session` / `crate::tool` / `crate::model` →
//!   `tiangong_core::session` / `tiangong_core::tool` / `tiangong_core::model`。
//! - `MessagePriority` / `AgentStatus` / `AgentDescriptor` / `AgentMessage` 改引自
//!   [`crate::state`]。
//! - `stream_tx` 仍为 `&StdSender<StreamEvent>`，由 handler.rs 桥接到 feedback_tx。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender as StdSender;

use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::{now_text, PendingPluginDelivery, Session};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
use tiangong_types::{
    content_blocks_are_empty, content_blocks_text, stable_content_blocks, ContentBlock, StreamEvent,
};

use crate::constants::{MAX_AGENTS, PLUGIN_ID};
use crate::state::message_bus::AgentInboxEntry;
use crate::state::{AgentDescriptor, AgentMessage, AgentStatus, MessagePriority};
use crate::TeamContext;

fn normalize_agent_role(role: &str) -> String {
    role.trim().trim_start_matches('@').to_string()
}

/// 团队工具消息使用调用尝试与目标派生的稳定 ID，确保 shutdown/重试不会生成
/// 第二份 durable 工作。极早期缺失 tool call ID 时回退到新 SCRU128。
fn team_tool_message_id(current_agent_id: &str, call: &ToolCall, target_agent_id: &str) -> String {
    if call.id.is_empty() {
        return scru128::new().to_string();
    }
    format!(
        "team-tool:{}:{}:{}:{}:{}:{}",
        current_agent_id.len(),
        current_agent_id,
        call.id.len(),
        call.id,
        target_agent_id.len(),
        target_agent_id
    )
}

pub(crate) fn prepared_agent_message_for_prompt(
    message: &AgentMessage,
    mut additional_content: Vec<ContentBlock>,
) -> Vec<ContentBlock> {
    let source_prefix = format!("[from:{} at {}]\n", message.from, message.created_at);
    if additional_content.is_empty() {
        return vec![ContentBlock::text(format!(
            "{source_prefix}{}",
            message.content
        ))];
    }

    if let Some(ContentBlock::Text { text }) = additional_content
        .iter_mut()
        .find(|block| matches!(block, ContentBlock::Text { .. }))
    {
        text.insert_str(0, &source_prefix);
    } else {
        additional_content.insert(0, ContentBlock::text(source_prefix));
    }
    additional_content
}
#[derive(Debug, Clone)]
struct RestoredAgent {
    agent_id: String,
    role: String,
    label: String,
    status: AgentStatus,
    system_prompt: Option<String>,
    tools: Option<Vec<String>>,
}

const AGENT_DESCRIPTOR_MARKER: &str = "tiangong-agent-descriptor:";

/// 从会话历史中的 Agent 事件恢复团队注册表。
///
/// GUI 的团队面板也是从同一批系统消息解析成员；Core 重建时必须同步恢复
/// TeamContext，否则界面显示有成员但团队工具会认为注册表为空。
pub fn restore_agents_from_session_history(
    team: &mut TeamContext,
    parent_session: &Session,
    parent_tools: &[ToolSpec],
    storage_root: &Path,
) -> usize {
    let mut restored: Vec<RestoredAgent> = Vec::new();

    for message in &parent_session.messages {
        if message.role != tiangong_core::session::MessageRole::System {
            continue;
        }

        if let Some(agent) = parse_persisted_agent_descriptor(message) {
            restored.retain(|existing| existing.role != agent.role);
            restored.push(agent);
            continue;
        }

        if let Some(agent) = parse_agent_created_message(&message.text_content()) {
            restored.retain(|existing| existing.role != agent.role);
            restored.push(agent);
            continue;
        }

        if let Some((label, status, agent_id)) = parse_agent_status_message(&message.text_content())
        {
            for agent in &mut restored {
                // 新日志带稳定 ID 时只能按 ID 匹配；label 仅用于兼容没有 ID 的旧日志。
                // 不同角色允许使用相同显示名，不能因其中一个被销毁而误删另一个。
                let matches = match agent_id.as_deref() {
                    Some(agent_id) => agent.agent_id == agent_id,
                    None => agent.label == label,
                };
                if matches {
                    agent.status = status.clone();
                }
            }
            restored.retain(|agent| agent.status != AgentStatus::Terminated);
        }
    }

    let excluded = ["create_agent", "dismiss_agent"];
    let tool_names: Vec<String> = parent_tools
        .iter()
        .map(|tool| tool.name.clone())
        .filter(|name| !excluded.contains(&name.as_str()))
        .collect();

    let mut count = 0usize;
    for agent in restored {
        if team.registry.find_by_role(&agent.role).is_some() {
            continue;
        }

        let descriptor = AgentDescriptor {
            agent_id: agent.agent_id.clone(),
            role: agent.role.clone(),
            label: agent.label.clone(),
            system_prompt: agent.system_prompt.unwrap_or_else(|| {
                format!(
                    "你是从会话历史恢复的子 Agent，角色为 {}（{}）。请延续当前会话上下文，按该角色职责处理用户和主 Agent 分配的任务。",
                    agent.label, agent.role
                )
            }),
            tools: agent.tools.unwrap_or_else(|| tool_names.clone()),
            status: AgentStatus::Idle,
        };
        let child_session = load_child_session(storage_root, parent_session, &agent.agent_id)
            .unwrap_or_else(|| create_child_session(parent_session, &agent.label, &agent.agent_id));
        team.registry
            .register_with_session(descriptor, child_session);
        count += 1;
    }

    count
}

/// 从会话历史提取最终处于显式 terminated 状态的稳定 Agent ID。
///
/// 用于恢复“解散记录已经落盘、父会话投递取消确认尚未来得及落盘”的崩溃窗口。
pub(crate) fn explicitly_terminated_agent_ids(
    parent_session: &Session,
) -> std::collections::HashSet<String> {
    let mut terminated = std::collections::HashSet::new();
    for message in &parent_session.messages {
        if message.role != tiangong_core::session::MessageRole::System {
            continue;
        }
        if let Some(agent) = parse_persisted_agent_descriptor(message) {
            terminated.remove(&agent.agent_id);
            continue;
        }
        if let Some(agent) = parse_agent_created_message(&message.text_content()) {
            terminated.remove(&agent.agent_id);
            continue;
        }
        if let Some((_label, status, Some(agent_id))) =
            parse_agent_status_message(&message.text_content())
        {
            if status == AgentStatus::Terminated {
                terminated.insert(agent_id);
            } else {
                terminated.remove(&agent_id);
            }
        }
    }
    terminated
}

fn parse_persisted_agent_descriptor(
    message: &tiangong_core::session::Message,
) -> Option<RestoredAgent> {
    message.content.iter().find_map(|block| {
        let ContentBlock::ModelInstruction { text } = block else {
            return None;
        };
        let json = text.strip_prefix(AGENT_DESCRIPTOR_MARKER)?;
        let descriptor = serde_json::from_str::<AgentDescriptor>(json).ok()?;
        Some(RestoredAgent {
            agent_id: descriptor.agent_id,
            role: descriptor.role,
            label: descriptor.label,
            status: descriptor.status,
            system_prompt: Some(descriptor.system_prompt),
            tools: Some(descriptor.tools),
        })
    })
}

fn parse_agent_created_message(content: &str) -> Option<RestoredAgent> {
    let rest = content.strip_prefix("[Agent] ")?;
    let (label, after_label) = rest.split_once(" (")?;
    let (role, after_role) = after_label.split_once(") 已加入团队")?;
    let agent_id = after_role
        .rsplit_once("id=")
        .map(|(_, id)| id.split_whitespace().next().unwrap_or_default().trim())?
        .to_string();
    if agent_id.is_empty() || role.trim().is_empty() || label.trim().is_empty() {
        return None;
    }

    Some(RestoredAgent {
        agent_id,
        role: role.trim().to_string(),
        label: label.trim().to_string(),
        status: AgentStatus::Idle,
        system_prompt: None,
        tools: None,
    })
}

fn parse_agent_status_message(content: &str) -> Option<(String, AgentStatus, Option<String>)> {
    let rest = content.strip_prefix("[Agent] ")?;
    let (label, after_label) = rest.split_once(" 状态变更: ")?;
    let status_text = after_label.split_whitespace().next().unwrap_or_default();
    let status = match status_text {
        "idle" => AgentStatus::Idle,
        "running" => AgentStatus::Running,
        "waiting_for_user" => AgentStatus::WaitingForUser,
        "waiting_for_lock" => AgentStatus::WaitingForLock,
        "terminated" => AgentStatus::Terminated,
        _ => return None,
    };
    let agent_id = after_label
        .rsplit_once("id=")
        .map(|(_, id)| {
            id.split_whitespace()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|id| !id.is_empty());
    Some((label.trim().to_string(), status, agent_id))
}

/// 处理 create_agent 工具调用
pub fn execute_create_agent(
    team: &mut TeamContext,
    call: &ToolCall,
    parent_session: &Session,
    parent_tools: &[ToolSpec],
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let role = call
        .arguments
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let label = call
        .arguments
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let system_prompt = call
        .arguments
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let requested_tools: Vec<String> = call
        .arguments
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if role.is_empty() || label.is_empty() || system_prompt.is_empty() {
        return error_tool_result("create_agent", "role、label 和 system_prompt 不能为空");
    }

    // 检查 role 是否已被占用
    if team.registry.find_by_role(&role).is_some() {
        return error_tool_result(
            "create_agent",
            &format!("角色 '{role}' 已被占用，请使用不同的 role"),
        );
    }

    // 检查 Agent 数量上限
    if team.registry.alive_count() >= MAX_AGENTS {
        return error_tool_result(
            "create_agent",
            &format!("团队 Agent 数量已达上限（{MAX_AGENTS}）"),
        );
    }

    // 确定工具集：如果指定了 tools 则过滤，否则继承全部（排除团队管理工具）
    let tool_names = if requested_tools.is_empty() {
        let excluded = ["create_agent", "dismiss_agent"];
        parent_tools
            .iter()
            .map(|t| t.name.clone())
            .filter(|n| !excluded.contains(&n.as_str()))
            .collect()
    } else {
        let available: Vec<&str> = parent_tools.iter().map(|t| t.name.as_str()).collect();
        requested_tools
            .into_iter()
            .filter(|t| available.contains(&t.as_str()))
            .collect()
    };

    let agent_id = scru128::new().to_string();

    let descriptor = AgentDescriptor {
        agent_id: agent_id.clone(),
        role: role.clone(),
        label: label.clone(),
        system_prompt,
        tools: tool_names,
        status: AgentStatus::Idle,
    };

    let child_session = create_child_session(parent_session, &label, &agent_id);

    // 注册
    team.registry
        .register_with_session(descriptor, child_session);

    // 发送事件
    let _ = stream_tx.send(StreamEvent::AgentCreated {
        agent_id: agent_id.clone(),
        role: role.clone(),
        label: label.clone(),
    });

    // 向所有已存在的 Agent 广播新成员通知
    let notification = format!("[团队通知] 新成员 {label} (@{role}) 已加入团队");
    let targets: Vec<String> = team
        .registry
        .alive_agents()
        .iter()
        .filter(|a| a.agent_id != agent_id)
        .map(|a| a.agent_id.clone())
        .collect();
    for target_id in targets {
        team.registry.deliver_message(
            &target_id,
            AgentMessage {
                id: scru128::new().to_string(),
                from: "system".to_string(),
                to: target_id.clone(),
                content: notification.clone(),
                priority: MessagePriority::Normal,
                created_at: now_text(),
            },
        );
    }

    let tools_list = team
        .registry
        .get(&agent_id)
        .map(|d| d.tools.join(", "))
        .unwrap_or_default();

    ToolResult {
        ok: true,
        summary: format!("{label} ({role}) 已加入团队"),
        stdout: format!(
            "Agent '{label}' (role={role}, id={agent_id}) 已创建。\n可用工具: {tools_list}\n状态: 等待任务分配"
        ),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: "create_agent".to_string(),
            args: vec![role, label],
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary: "Agent 已加入团队".to_string(),
        }),
    }
}

/// 处理 dismiss_agent 工具调用
pub(crate) fn agent_is_running(team: &TeamContext, descriptor: &AgentDescriptor) -> bool {
    descriptor.status == AgentStatus::Running || team.is_agent_active(&descriptor.agent_id)
}

pub fn execute_dismiss_agent(
    team: &mut TeamContext,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let role = call
        .arguments
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if role.is_empty() {
        return error_tool_result("dismiss_agent", "role 不能为空");
    }

    let Some(descriptor) = team.registry.find_by_role(&role).cloned() else {
        return error_tool_result("dismiss_agent", &format!("未找到角色 '{role}'"));
    };

    if agent_is_running(team, &descriptor) {
        return error_tool_result(
            "dismiss_agent",
            &format!(
                "Agent '{}' 正在运行，请先取消该 Agent，等待任务收敛后再重试解散",
                descriptor.label
            ),
        );
    }

    let agent_id = descriptor.agent_id.clone();
    let label = descriptor.label.clone();

    // 释放文件锁
    for path in team.file_locks.release_all(&agent_id) {
        let _ = stream_tx.send(StreamEvent::FileLockChanged {
            path,
            holder_agent_id: Some(agent_id.clone()),
            holder_agent_label: Some(label.clone()),
            action: "unlocked".to_string(),
        });
    }

    // 注销
    team.registry.unregister(&agent_id);

    let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
        agent_id: agent_id.clone(),
        label: label.clone(),
        status: "terminated".to_string(),
    });

    ToolResult {
        ok: true,
        summary: format!("{label} 已解散"),
        stdout: format!("Agent '{label}' (role={role}) 已解散，所有资源已释放"),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: "dismiss_agent".to_string(),
            args: vec![role],
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary: format!("{label} 已解散"),
        }),
    }
}

/// 处理 send_message 工具调用
pub fn execute_send_message(
    team: &mut TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let to = normalize_agent_role(
        call.arguments
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    );
    let content = call
        .arguments
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if to.is_empty() || content.is_empty() {
        return error_tool_result("send_message", "to 和 content 不能为空");
    }

    let from_label = team
        .registry
        .get(current_agent_id)
        .map(|d| d.label.as_str())
        .unwrap_or("Main Agent")
        .to_string();

    if to == "main" {
        let message = AgentMessage {
            id: team_tool_message_id(current_agent_id, call, "main"),
            from: current_agent_id.to_string(),
            to: "main".to_string(),
            content: content.clone(),
            priority: MessagePriority::Normal,
            created_at: now_text(),
        };
        team.deliver_main_message(message);
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: current_agent_id.to_string(),
            from_agent_label: from_label.clone(),
            to_agent_id: "main".to_string(),
            to_agent_label: "Main Agent".to_string(),
            content: content.clone(),
        });
        return ToolResult {
            ok: true,
            summary: "消息已发送给 Main Agent".to_string(),
            stdout: "消息已送达 → @main (Main Agent)".to_string(),
            stderr: String::new(),
            exit_code: 0,
            execution: Some(ToolExecutionRecord {
                tool_name: "send_message".to_string(),
                args: vec![to, content.chars().take(100).collect()],
                duration_ms: 0,
                ok: true,
                exit_code: 0,
                summary: "消息已发送给 Main Agent".to_string(),
            }),
        };
    }

    let Some(target) = team.registry.find_by_role(&to).cloned() else {
        return error_tool_result("send_message", &format!("未找到角色 '{to}'"));
    };

    let message = AgentMessage {
        id: team_tool_message_id(current_agent_id, call, &target.agent_id),
        from: current_agent_id.to_string(),
        to: target.agent_id.clone(),
        content: content.clone(),
        priority: MessagePriority::Normal,
        created_at: now_text(),
    };

    team.registry.deliver_message(&target.agent_id, message);

    let _ = stream_tx.send(StreamEvent::AgentMessage {
        from_agent_id: current_agent_id.to_string(),
        from_agent_label: from_label.clone(),
        to_agent_id: target.agent_id.clone(),
        to_agent_label: target.label.clone(),
        content: content.clone(),
    });

    ToolResult {
        ok: true,
        summary: format!("消息已发送给 {label}", label = target.label),
        stdout: format!("消息已送达 → @{to} ({label})", label = target.label),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: "send_message".to_string(),
            args: vec![to, content.chars().take(100).collect()],
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary: format!("消息已发送给 {}", target.label),
        }),
    }
}

/// 处理 broadcast_message 工具调用
pub fn execute_broadcast_message(
    team: &mut TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let content = call
        .arguments
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let exclude: Vec<String> = call
        .arguments
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(normalize_agent_role))
                .filter(|role| !role.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if content.is_empty() {
        return error_tool_result("broadcast_message", "content 不能为空");
    }

    let from_label = team
        .registry
        .get(current_agent_id)
        .map(|d| d.label.as_str())
        .unwrap_or("Main Agent")
        .to_string();

    let exclude_ids: Vec<String> = if exclude.is_empty() {
        vec![current_agent_id.to_string()]
    } else {
        exclude
            .iter()
            .filter_map(|r| team.registry.find_by_role(r).map(|a| a.agent_id.clone()))
            .collect()
    };

    let created_at = now_text();

    let event_targets: Vec<_> = {
        let targets = team.registry.alive_agents();
        targets
            .iter()
            .filter(|t| !exclude_ids.iter().any(|id| id == &t.agent_id))
            .map(|t| (t.agent_id.clone(), t.label.clone()))
            .collect()
    };
    let target_count = event_targets.len();

    let targets = event_targets
        .iter()
        .map(|(agent_id, _)| agent_id.clone())
        .collect::<Vec<_>>();
    for agent_id in &targets {
        let msg = AgentMessage {
            id: team_tool_message_id(current_agent_id, call, agent_id),
            from: current_agent_id.to_string(),
            to: agent_id.clone(),
            content: content.clone(),
            priority: MessagePriority::Normal,
            created_at: created_at.clone(),
        };
        team.registry.deliver_message(agent_id, msg);
    }

    // 为每个目标发送事件
    for (agent_id, agent_label) in event_targets {
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: current_agent_id.to_string(),
            from_agent_label: from_label.clone(),
            to_agent_id: agent_id,
            to_agent_label: agent_label,
            content: content.clone(),
        });
    }

    ToolResult {
        ok: true,
        summary: format!("广播已发送给 {target_count} 个 Agent"),
        stdout: format!("广播消息已送达 → @all ({target_count} 个 Agent)"),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: "broadcast_message".to_string(),
            args: vec![content.chars().take(100).collect()],
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary: format!("广播已发送给 {target_count} 个 Agent"),
        }),
    }
}

/// 处理 notify_user 工具调用
pub fn execute_notify_user(
    team: &TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let content = call
        .arguments
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let level = call
        .arguments
        .get("level")
        .and_then(|v| v.as_str())
        .unwrap_or("info")
        .trim()
        .to_string();

    if content.is_empty() {
        return error_tool_result("notify_user", "content 不能为空");
    }

    let agent_label = team
        .registry
        .get(current_agent_id)
        .map(|d| d.label.as_str())
        .unwrap_or("Main Agent")
        .to_string();

    let _ = stream_tx.send(StreamEvent::AgentNotification {
        agent_id: current_agent_id.to_string(),
        agent_label: agent_label.clone(),
        content: content.clone(),
        level: level.clone(),
    });

    ToolResult {
        ok: true,
        summary: "通知已推送给用户".to_string(),
        stdout: format!("已推送给用户 [{level}]: {content}"),
        stderr: String::new(),
        exit_code: 0,
        execution: Some(ToolExecutionRecord {
            tool_name: "notify_user".to_string(),
            args: vec![level, content.chars().take(100).collect()],
            duration_ms: 0,
            ok: true,
            exit_code: 0,
            summary: "通知已推送".to_string(),
        }),
    }
}

/// 处理 lock_file 工具调用
pub fn execute_lock_file(
    team: &mut TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let path = call
        .arguments
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if path.is_empty() {
        return error_tool_result("lock_file", "path 不能为空");
    }

    let path_buf = PathBuf::from(&path);
    let now = chrono::Local::now().naive_local();

    if let Some(expired) = team.file_locks.take_expired(&path_buf, &now) {
        let holder_agent_label = team
            .registry
            .get(&expired.holder)
            .map(|descriptor| descriptor.label.clone());
        let _ = stream_tx.send(StreamEvent::FileLockChanged {
            path: path.clone(),
            holder_agent_id: Some(expired.holder),
            holder_agent_label,
            action: "expired".to_string(),
        });
    }

    match team.file_locks.try_lock(&path_buf, current_agent_id, &now) {
        Ok(()) => {
            let holder_label = team
                .registry
                .get(current_agent_id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| "Main Agent".to_string());
            let _ = stream_tx.send(StreamEvent::FileLockChanged {
                path: path.clone(),
                holder_agent_id: Some(current_agent_id.to_string()),
                holder_agent_label: Some(holder_label),
                action: "locked".to_string(),
            });
            ToolResult {
                ok: true,
                summary: format!("文件锁已获取: {path}"),
                stdout: format!("已锁定 {path}"),
                stderr: String::new(),
                exit_code: 0,
                execution: Some(ToolExecutionRecord {
                    tool_name: "lock_file".to_string(),
                    args: vec![path],
                    duration_ms: 0,
                    ok: true,
                    exit_code: 0,
                    summary: "文件锁已获取".to_string(),
                }),
            }
        }
        Err(err) => ToolResult {
            ok: false,
            summary: format!("获取文件锁失败: {err}"),
            stdout: String::new(),
            stderr: err,
            exit_code: 1,
            execution: Some(ToolExecutionRecord {
                tool_name: "lock_file".to_string(),
                args: vec![path],
                duration_ms: 0,
                ok: false,
                exit_code: 1,
                summary: "获取文件锁失败".to_string(),
            }),
        },
    }
}

/// 处理 unlock_file 工具调用
pub fn execute_unlock_file(
    team: &mut TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    let path = call
        .arguments
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if path.is_empty() {
        return error_tool_result("unlock_file", "path 不能为空");
    }

    let path_buf = PathBuf::from(&path);
    let holder_agent_id = team.file_locks.holder(&path_buf).map(str::to_string);
    let holder_agent_label = holder_agent_id.as_deref().and_then(|holder| {
        team.registry
            .get(holder)
            .map(|descriptor| descriptor.label.clone())
    });

    let result = if current_agent_id == "main" {
        team.file_locks.force_unlock(&path_buf);
        Ok(())
    } else {
        team.file_locks.unlock(&path_buf, current_agent_id)
    };

    match result {
        Ok(()) => {
            let _ = stream_tx.send(StreamEvent::FileLockChanged {
                path: path.clone(),
                holder_agent_id,
                holder_agent_label,
                action: "unlocked".to_string(),
            });
            ToolResult {
                ok: true,
                summary: format!("文件锁已释放: {path}"),
                stdout: format!("已解锁 {path}"),
                stderr: String::new(),
                exit_code: 0,
                execution: Some(ToolExecutionRecord {
                    tool_name: "unlock_file".to_string(),
                    args: vec![path],
                    duration_ms: 0,
                    ok: true,
                    exit_code: 0,
                    summary: "文件锁已释放".to_string(),
                }),
            }
        }
        Err(err) => ToolResult {
            ok: false,
            summary: format!("释放文件锁失败: {err}"),
            stdout: String::new(),
            stderr: err,
            exit_code: 1,
            execution: Some(ToolExecutionRecord {
                tool_name: "unlock_file".to_string(),
                args: vec![path],
                duration_ms: 0,
                ok: false,
                exit_code: 1,
                summary: "释放文件锁失败".to_string(),
            }),
        },
    }
}

/// 统一处理团队工具调用
pub fn execute_team_tool(
    team: &mut TeamContext,
    current_agent_id: &str,
    call: &ToolCall,
    parent_session: &mut Session,
    parent_tools: &[ToolSpec],
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    match call.name.as_str() {
        "create_agent" => {
            let result = execute_create_agent(team, call, parent_session, parent_tools, stream_tx);
            if result.ok {
                if let Some(role) = call.arguments.get("role").and_then(|value| value.as_str()) {
                    if let Some(agent) = team.registry.find_by_role(role) {
                        append_team_history_message(
                            parent_session,
                            stream_tx,
                            format!(
                                "[Agent] {} ({}) 已加入团队 id={}",
                                agent.label, agent.role, agent.agent_id
                            ),
                            Some(agent),
                        );
                    }
                }
            }
            result
        }
        "dismiss_agent" => {
            let previous = call
                .arguments
                .get("role")
                .and_then(|value| value.as_str())
                .and_then(|role| team.registry.find_by_role(role))
                .map(|agent| (agent.agent_id.clone(), agent.label.clone()));
            let result = execute_dismiss_agent(team, call, stream_tx);
            if result.ok {
                if let Some((agent_id, label)) = previous {
                    append_team_history_message(
                        parent_session,
                        stream_tx,
                        format!("[Agent] {label} 状态变更: terminated id={agent_id}"),
                        None,
                    );
                }
            }
            result
        }
        "send_message" => execute_send_message(team, current_agent_id, call, stream_tx),
        "broadcast_message" => execute_broadcast_message(team, current_agent_id, call, stream_tx),
        "notify_user" => execute_notify_user(team, current_agent_id, call, stream_tx),
        "lock_file" => execute_lock_file(team, current_agent_id, call, stream_tx),
        "unlock_file" => execute_unlock_file(team, current_agent_id, call, stream_tx),
        _ => error_tool_result(&call.name, &format!("未知团队工具: {}", call.name)),
    }
}

fn append_team_history_message(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    content: String,
    descriptor: Option<&AgentDescriptor>,
) {
    let mut message =
        tiangong_core::session::Message::new(tiangong_core::session::MessageRole::System, content);
    message.model_excluded = true;
    if let Some(descriptor) = descriptor {
        if let Ok(json) = serde_json::to_string(descriptor) {
            message
                .content
                .push(ContentBlock::model_instruction(format!(
                    "{AGENT_DESCRIPTOR_MARKER}{json}"
                )));
        }
    }
    let message_id = message.id.clone();
    session.messages.push(message);
    if let Err(error) = session.try_persist_to_disk() {
        tracing::warn!(%error, "持久化 Agent 生命周期记录失败");
    }
    if let Some(mut message) = session
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
    {
        message.clear_transient_data();
        let _ = stream_tx.send(StreamEvent::SessionMessageUpsert {
            message,
            pending_plugin_deliveries: None,
            completed_plugin_delivery_ids: None,
            deferred_tool_injections: None,
        });
    }
}

/// 用户输入中的 Agent @ 路由结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMentionRoute {
    pub roles: Vec<String>,
    pub broadcast: bool,
    pub content: String,
}

/// 解析用户消息开头的 Agent @提及。
pub fn parse_agent_mention_route(content: &str) -> Option<AgentMentionRoute> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('@') {
        return None;
    }

    let mut roles = Vec::new();
    let mut broadcast = false;
    let mut rest = trimmed;

    while let Some(after_at) = rest.strip_prefix('@') {
        let mention_len = after_at
            .char_indices()
            .find_map(|(idx, ch)| {
                (!(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')).then_some(idx)
            })
            .unwrap_or(after_at.len());
        if mention_len == 0 {
            break;
        }
        let mention = &after_at[..mention_len];
        if mention == "all" {
            broadcast = true;
        } else {
            roles.push(mention.to_string());
        }
        rest = &after_at[mention_len..];
        let next = rest.trim_start();
        if next.len() == rest.len() || !next.starts_with('@') {
            rest = next;
            break;
        }
        rest = next;
    }

    if !broadcast && roles.is_empty() {
        return None;
    }

    Some(AgentMentionRoute {
        roles,
        broadcast,
        content: rest.trim_start().to_string(),
    })
}

/// 将用户 @提及消息直接投递给目标 Agent。
pub fn route_user_mentions(
    team: &mut TeamContext,
    content: &str,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    route_user_mentions_with_content(team, None, vec![ContentBlock::text(content)], stream_tx)
}

/// 在用户消息持久化前生成确定性的直达投递计划。
pub(crate) fn plan_user_mention_deliveries(
    team: &TeamContext,
    source_message_id: &str,
    prepared: &[ContentBlock],
) -> Vec<PendingPluginDelivery> {
    build_user_mention_entries(team, Some(source_message_id), prepared.to_vec())
        .into_iter()
        .map(|(target_agent_id, _label, entry)| PendingPluginDelivery {
            delivery_id: entry.message.id,
            source_message_id: source_message_id.to_string(),
            plugin_id: PLUGIN_ID.to_string(),
            target_id: target_agent_id,
            content: entry.message.content,
            created_at: entry.message.created_at,
            additional_content: entry.additional_content,
        })
        .collect()
}

/// 把已经随父 Session 落盘的投递计划放入内存收件箱。
pub(crate) fn dispatch_pending_agent_deliveries(
    team: &mut TeamContext,
    parent_session: &Session,
    source_message_id: &str,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    team.registry
        .remove_pending_source_message(source_message_id);
    let deliveries = parent_session
        .pending_plugin_deliveries
        .iter()
        .filter(|delivery| {
            (delivery.plugin_id.is_empty() || delivery.plugin_id == PLUGIN_ID)
                && delivery.source_message_id == source_message_id
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut dispatched = false;
    for delivery in deliveries {
        let Some(agent) = team.registry.get(&delivery.target_id) else {
            continue;
        };
        if agent.status == AgentStatus::Terminated {
            continue;
        }
        let label = agent.label.clone();
        let content = delivery.content.clone();
        team.registry.deliver_inbox_entry(
            &delivery.target_id,
            AgentInboxEntry {
                message: AgentMessage {
                    id: delivery.delivery_id,
                    from: "user".to_string(),
                    to: delivery.target_id.clone(),
                    content: delivery.content,
                    priority: MessagePriority::Normal,
                    created_at: delivery.created_at,
                },
                additional_content: delivery.additional_content,
                session_message_id: Some(delivery.source_message_id),
            },
        );
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: "user".to_string(),
            from_agent_label: "User".to_string(),
            to_agent_id: delivery.target_id,
            to_agent_label: label,
            content,
        });
        dispatched = true;
    }
    dispatched
}

/// Core 重建后恢复尚未完成的直达投递；注册表按 delivery id 幂等去重。
pub(crate) fn restore_pending_agent_deliveries(
    team: &mut TeamContext,
    parent_session: &Session,
) -> usize {
    let mut restored = 0usize;
    for delivery in &parent_session.pending_plugin_deliveries {
        if !delivery.plugin_id.is_empty() && delivery.plugin_id != PLUGIN_ID {
            continue;
        }
        let Some(agent) = team.registry.get(&delivery.target_id) else {
            continue;
        };
        if agent.status == AgentStatus::Terminated {
            continue;
        }
        team.registry.deliver_inbox_entry(
            &delivery.target_id,
            AgentInboxEntry {
                message: AgentMessage {
                    id: delivery.delivery_id.clone(),
                    from: "user".to_string(),
                    to: delivery.target_id.clone(),
                    content: delivery.content.clone(),
                    priority: MessagePriority::Normal,
                    created_at: delivery.created_at.clone(),
                },
                additional_content: delivery.additional_content.clone(),
                session_message_id: Some(delivery.source_message_id.clone()),
            },
        );
        restored += 1;
    }
    restored
}

/// 将用户 @提及消息投递给目标 Agent。运行中的 Agent 会实时注入当前循环，
/// 空闲 Agent 会进入收件箱并唤醒调度器启动新一轮执行。
pub fn route_user_mentions_with_content(
    team: &mut TeamContext,
    source_message_id: Option<&str>,
    prepared: Vec<ContentBlock>,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    let entries = build_user_mention_entries(team, source_message_id, prepared);
    if entries.is_empty() {
        return false;
    }
    for (agent_id, label, entry) in entries {
        let content = entry.message.content.clone();
        team.registry.deliver_inbox_entry(&agent_id, entry);
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: "user".to_string(),
            from_agent_label: "User".to_string(),
            to_agent_id: agent_id,
            to_agent_label: label,
            content,
        });
    }
    true
}

fn build_user_mention_entries(
    team: &TeamContext,
    source_message_id: Option<&str>,
    prepared: Vec<ContentBlock>,
) -> Vec<(String, String, AgentInboxEntry)> {
    let content = content_blocks_text(&prepared);
    let Some(route) = parse_agent_mention_route(&content) else {
        return Vec::new();
    };
    let mut targets: Vec<_> = if route.broadcast {
        team.registry
            .alive_agents()
            .iter()
            .map(|agent| {
                (
                    agent.agent_id.clone(),
                    agent.role.clone(),
                    agent.label.clone(),
                )
            })
            .collect()
    } else {
        route
            .roles
            .iter()
            .filter_map(|role| {
                team.registry.find_by_role(role).map(|agent| {
                    (
                        agent.agent_id.clone(),
                        agent.role.clone(),
                        agent.label.clone(),
                    )
                })
            })
            .collect()
    };
    let mut seen_targets = std::collections::HashSet::new();
    targets.retain(|(agent_id, _, _)| seen_targets.insert(agent_id.clone()));

    // 只移除开头的 @Agent 路由前缀，完整保留宿主已准备内容块的相对顺序。
    let mut routed_content = prepared;
    let mut prefix_bytes = content.len().saturating_sub(route.content.len());
    for block in &mut routed_content {
        let ContentBlock::Text { text } = block else {
            continue;
        };
        if prefix_bytes == 0 {
            break;
        }
        if prefix_bytes >= text.len() {
            prefix_bytes -= text.len();
            text.clear();
        } else {
            debug_assert!(text.is_char_boundary(prefix_bytes));
            text.drain(..prefix_bytes);
            prefix_bytes = 0;
        }
    }

    let routed_content = stable_content_blocks(&routed_content);
    if content_blocks_are_empty(&routed_content) {
        return Vec::new();
    }
    let routed_revision = serde_json::to_string(&routed_content).unwrap_or_default();
    targets
        .into_iter()
        .map(|(agent_id, _role, label)| {
            let delivery_id = source_message_id.map_or_else(
                || scru128::new().to_string(),
                |source| {
                    let mut hasher = DefaultHasher::new();
                    source.hash(&mut hasher);
                    agent_id.hash(&mut hasher);
                    routed_revision.hash(&mut hasher);
                    format!("{source}:{agent_id}:{:016x}", hasher.finish())
                },
            );
            let message = AgentMessage {
                id: delivery_id,
                from: "user".to_string(),
                to: agent_id.clone(),
                content: route.content.clone(),
                priority: MessagePriority::Normal,
                created_at: now_text(),
            };
            (
                agent_id,
                label,
                AgentInboxEntry {
                    message,
                    additional_content: routed_content.clone(),
                    session_message_id: source_message_id.map(str::to_owned),
                },
            )
        })
        .collect()
}

pub fn error_tool_result(name: &str, message: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: format!("{name} 失败: {message}"),
        stdout: String::new(),
        stderr: message.to_string(),
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: name.to_string(),
            args: Vec::new(),
            duration_ms: 0,
            ok: false,
            exit_code: 1,
            summary: format!("{name} 失败"),
        }),
    }
}

/// 创建 Sub Agent 的独立 Session
///
/// `agent_id` 写入 `child.active_agent_id`，供会话恢复和前端状态归属使用；
/// 工具调用方身份始终由 ReactEngine 显式传入。
fn create_child_session(parent: &Session, title: &str, agent_id: &str) -> Session {
    let mut child = Session::new(title);
    child.cwd = parent.cwd.clone();
    child.reasoning_effort = parent.reasoning_effort.clone();
    child.parent_session_id = Some(parent.id.clone());
    child.active_agent_id = Some(agent_id.to_string());
    child
}

/// 持久化 child_session 到磁盘
pub fn persist_child_session(
    storage_root: &Path,
    parent_session: &Session,
    agent_id: &str,
    session: &Session,
) -> Result<(), String> {
    persist_child_session_for_parent_id(storage_root, &parent_session.id, agent_id, session)
}

/// 仅使用宿主注入的存储根与稳定父会话 ID 保存 child session。
///
/// 后台回执确认没有完整父 Session，也不应为计算路径而构造依赖 Core 全局存储的
/// 临时 Session；插件自己的持久化路径由这两个显式参数唯一决定。
pub fn persist_child_session_for_parent_id(
    storage_root: &Path,
    parent_session_id: &str,
    agent_id: &str,
    session: &Session,
) -> Result<(), String> {
    let agents_dir = storage_root
        .join("sessions")
        .join(parent_session_id)
        .join("agents");
    std::fs::create_dir_all(&agents_dir)
        .map_err(|error| format!("创建 agents 目录失败：{error}"))?;
    let path = agents_dir.join(format!("{agent_id}.json"));
    let content = serde_json::to_string_pretty(session)
        .map_err(|error| format!("child session 序列化失败：{error}"))?;
    tiangong_core::session::atomic_replace_file(&path, content.as_bytes())
        .map_err(|error| format!("child session 持久化写入失败：{error}"))
}

/// 从磁盘加载 child_session
pub fn load_child_session(
    storage_root: &Path,
    parent_session: &Session,
    agent_id: &str,
) -> Option<Session> {
    let path = storage_root
        .join("sessions")
        .join(&parent_session.id)
        .join("agents")
        .join(format!("{agent_id}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    let mut session: Session = serde_json::from_str(&content).ok()?;
    // 确保从磁盘恢复的 child session 带有正确的 active_agent_id（旧版本持久化
    // 的 session 可能缺失该字段）。
    session.active_agent_id = Some(agent_id.to_string());
    Some(session)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};

    use serde_json::json;

    use super::*;

    /// 创建独立存储根，供触及 child session 持久化的用例使用。
    /// 返回的 TempDir 须保持存活到用例结束。
    fn isolated_storage_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("创建临时目录失败");
        // execute_team_tool 仍通过真实 Session 持久化团队历史；测试需像宿主一样
        // 注入 Core 存储根，同时 child session 继续显式使用 root.path()。
        tiangong_core::storage::set_storage_root(root.path().to_path_buf());
        root
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: scru128::new().to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
        }
    }

    #[test]
    fn create_agent_keeps_child_session_with_parent_cwd() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.cwd = "/tmp/workspace".to_string();

        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "You are a developer"
                }),
            ),
            &parent,
            &[tool("read_file"), tool("write_file")],
            &tx,
        );

        assert!(result.ok);
        let agent = team.registry.find_by_role("dev").expect("agent exists");
        let child = team
            .registry
            .get_session(&agent.agent_id)
            .expect("child session is persisted");
        assert_eq!(child.cwd, parent.cwd);
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn sub_agent_send_message_routes_from_current_agent_identity() {
        let (tx, rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let tools = [tool("send_message")];

        for (role, label) in [("dev", "Developer"), ("test", "Tester")] {
            let result = execute_create_agent(
                &mut team,
                &call(
                    "create_agent",
                    json!({
                        "role": role,
                        "label": label,
                        "system_prompt": "agent"
                    }),
                ),
                &parent,
                &tools,
                &tx,
            );
            assert!(result.ok);
        }

        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let test_id = team.registry.find_by_role("test").unwrap().agent_id.clone();
        let result = execute_send_message(
            &mut team,
            &dev_id,
            &call(
                "send_message",
                json!({
                    "to": "test",
                    "content": "please verify"
                }),
            ),
            &tx,
        );

        assert!(result.ok);
        let inbox = team.registry.drain_inbox(&test_id);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, dev_id);
        assert_eq!(inbox[0].to, test_id);
        assert_eq!(inbox[0].content, "please verify");
        assert!(rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::AgentMessage {
                from_agent_label,
                to_agent_label,
                ..
            } if from_agent_label == "Developer" && to_agent_label == "Tester"
        )));
    }

    #[test]
    fn send_message_accepts_at_prefixed_role_alias() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let tools = [tool("send_message")];

        for (role, label) in [("dev", "Developer"), ("test", "Tester")] {
            let result = execute_create_agent(
                &mut team,
                &call(
                    "create_agent",
                    json!({
                        "role": role,
                        "label": label,
                        "system_prompt": "agent"
                    }),
                ),
                &parent,
                &tools,
                &tx,
            );
            assert!(result.ok);
        }

        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let test_id = team.registry.find_by_role("test").unwrap().agent_id.clone();
        let result = execute_send_message(
            &mut team,
            &dev_id,
            &call(
                "send_message",
                json!({
                    "to": "@tester",
                    "content": "please verify"
                }),
            ),
            &tx,
        );

        assert!(result.ok);
        let inbox = team.registry.drain_inbox(&test_id);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, dev_id);
        assert_eq!(inbox[0].to, test_id);
        assert_eq!(inbox[0].content, "please verify");
    }

    #[test]
    fn broadcast_message_accepts_at_prefixed_exclude_roles() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let tools = [tool("broadcast_message")];

        for (role, label) in [("dev", "Developer"), ("test", "Tester"), ("pm", "PM")] {
            let result = execute_create_agent(
                &mut team,
                &call(
                    "create_agent",
                    json!({
                        "role": role,
                        "label": label,
                        "system_prompt": "agent"
                    }),
                ),
                &parent,
                &tools,
                &tx,
            );
            assert!(result.ok);
        }

        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let test_id = team.registry.find_by_role("test").unwrap().agent_id.clone();
        let pm_id = team.registry.find_by_role("pm").unwrap().agent_id.clone();

        // 先清空团队通知（创建 Agent 时自动广播的）
        let _ = team.registry.drain_inbox(&dev_id);
        let _ = team.registry.drain_inbox(&test_id);
        let _ = team.registry.drain_inbox(&pm_id);

        let broadcast_call = call(
            "broadcast_message",
            json!({
                "content": "sync status",
                "exclude": ["@dev"]
            }),
        );
        let result = execute_broadcast_message(&mut team, &dev_id, &broadcast_call, &tx);

        assert!(result.ok);
        assert!(team.registry.drain_inbox(&dev_id).is_empty());
        let test_inbox = team.registry.drain_inbox(&test_id);
        let pm_inbox = team.registry.drain_inbox(&pm_id);
        assert_eq!(test_inbox.len(), 1);
        assert_eq!(pm_inbox.len(), 1);

        let test_delivery_id = test_inbox[0].id.clone();
        let pm_delivery_id = pm_inbox[0].id.clone();
        assert_ne!(test_delivery_id, pm_delivery_id);
        assert_eq!(test_inbox[0].from, dev_id);
        assert_eq!(test_inbox[0].to, test_id);
        assert_eq!(pm_inbox[0].from, dev_id);
        assert_eq!(pm_inbox[0].to, pm_id);
        assert_eq!(test_inbox[0].content, pm_inbox[0].content);
        assert_eq!(test_inbox[0].priority, pm_inbox[0].priority);
        assert_eq!(test_inbox[0].created_at, pm_inbox[0].created_at);

        let retry = execute_broadcast_message(&mut team, &dev_id, &broadcast_call, &tx);
        assert!(retry.ok);
        assert!(team.registry.drain_inbox(&dev_id).is_empty());
        let retried_test = team.registry.drain_inbox(&test_id);
        let retried_pm = team.registry.drain_inbox(&pm_id);
        assert_eq!(retried_test[0].id, test_delivery_id);
        assert_eq!(retried_pm[0].id, pm_delivery_id);

        let protocol_root = tempfile::tempdir().unwrap();
        let protocol_store = crate::cancellation::CancellationTombstoneStore::new(
            protocol_root.path().to_path_buf(),
        );
        let state = protocol_store
            .settle("parent", std::slice::from_ref(&test_delivery_id))
            .unwrap();
        assert!(state.settled_ids.contains(&test_delivery_id));
        assert!(!state.settled_ids.contains(&pm_delivery_id));

        let state = protocol_store
            .settle("parent", std::slice::from_ref(&pm_delivery_id))
            .unwrap();
        assert!(state.settled_ids.contains(&test_delivery_id));
        assert!(state.settled_ids.contains(&pm_delivery_id));
    }

    #[test]
    fn sub_agent_can_notify_main_agent_inbox() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "agent"
                }),
            ),
            &parent,
            &[tool("send_message")],
            &tx,
        );
        assert!(result.ok);
        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();

        let result = execute_send_message(
            &mut team,
            &dev_id,
            &call(
                "send_message",
                json!({
                    "to": "main",
                    "content": "实现完成，请汇总"
                }),
            ),
            &tx,
        );

        assert!(result.ok);
        let messages = team.drain_main_inbox();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, dev_id);
        assert_eq!(messages[0].to, "main");
        assert_eq!(messages[0].content, "实现完成，请汇总");
    }

    #[test]
    fn sub_agent_write_requires_own_file_lock_and_emits_lock_events() {
        let (tx, rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let now = chrono::Local::now().naive_local();
        let path = PathBuf::from("src/auth.rs");

        assert!(team
            .file_locks
            .ensure_can_write(&path, "dev-agent", &now)
            .is_err());

        let result = execute_lock_file(
            &mut team,
            "dev-agent",
            &call("lock_file", json!({"path": "src/auth.rs"})),
            &tx,
        );
        assert!(result.ok);
        assert!(team
            .file_locks
            .ensure_can_write(&path, "dev-agent", &now)
            .is_ok());
        assert!(team
            .file_locks
            .ensure_can_write(&path, "test-agent", &now)
            .is_err());
        assert!(rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::FileLockChanged {
                action,
                holder_agent_id,
                ..
            } if action == "locked" && holder_agent_id.as_deref() == Some("dev-agent")
        )));
    }

    #[test]
    fn dismiss_agent_releases_owned_locks_with_events() {
        let (tx, rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "agent"
                }),
            ),
            &parent,
            &[tool("lock_file")],
            &tx,
        );
        assert!(result.ok);
        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        assert!(
            execute_lock_file(
                &mut team,
                &dev_id,
                &call("lock_file", json!({"path": "src/lib.rs"})),
                &tx,
            )
            .ok
        );

        let result = execute_dismiss_agent(
            &mut team,
            &call("dismiss_agent", json!({"role": "dev"})),
            &tx,
        );
        assert!(result.ok);
        assert!(team.file_locks.active_locks_summary().is_empty());
        assert!(rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::FileLockChanged {
                action,
                holder_agent_id: Some(holder),
                ..
            } if action == "unlocked" && holder == dev_id
        )));
    }

    #[test]
    fn dismiss_running_agent_keeps_descriptor_and_owned_locks() {
        let (tx, rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "agent"
                }),
            ),
            &parent,
            &[tool("lock_file")],
            &tx,
        );
        assert!(result.ok);

        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let locked_path = PathBuf::from("src/lib.rs");
        assert!(
            execute_lock_file(
                &mut team,
                &dev_id,
                &call("lock_file", json!({"path": locked_path})),
                &tx,
            )
            .ok
        );
        team.registry.update_status(&dev_id, AgentStatus::Running);
        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        team.register_active_agent(
            dev_id.clone(),
            command_tx,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Some("delivery-1".to_string()),
        );
        for _ in rx.try_iter() {}

        let result = execute_dismiss_agent(
            &mut team,
            &call("dismiss_agent", json!({"role": "dev"})),
            &tx,
        );

        assert!(!result.ok);
        assert!(result.stderr.contains("请先取消"));
        assert!(result.stderr.contains("等待任务收敛后再重试"));
        assert_eq!(
            team.registry.find_by_role("dev").map(|agent| &agent.status),
            Some(&AgentStatus::Running)
        );
        assert!(team.is_agent_active(&dev_id));
        assert_eq!(team.file_locks.holder(&locked_path), Some(dev_id.as_str()));
        assert!(!rx.try_iter().any(|event| matches!(
            &event,
            StreamEvent::FileLockChanged { action, .. } if action == "unlocked"
        ) || matches!(
            &event,
            StreamEvent::AgentStatusChanged { status, .. } if status == "terminated"
        )));
    }

    #[test]
    fn restored_agent_from_session_history_can_be_dismissed() {
        let root = isolated_storage_root();
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] 开发者 (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] 项目经理 (pm) 已加入团队 id=agent-pm".to_string(),
        );

        let restored = restore_agents_from_session_history(
            &mut team,
            &parent,
            &[
                tool("read_file"),
                tool("dismiss_agent"),
                tool("create_agent"),
            ],
            root.path(),
        );
        assert_eq!(restored, 2);
        assert!(team.registry.find_by_role("dev").is_some());
        assert!(team.registry.find_by_role("pm").is_some());

        let result = execute_dismiss_agent(
            &mut team,
            &call("dismiss_agent", json!({"role": "dev"})),
            &tx,
        );
        assert!(result.ok);
        assert!(team.registry.find_by_role("dev").is_none());
        assert!(team.registry.find_by_role("pm").is_some());
    }

    #[test]
    fn restore_agents_ignores_terminated_history_entries() {
        let root = isolated_storage_root();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] 开发者 (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] 开发者 状态变更: terminated id=agent-dev".to_string(),
        );

        let restored = restore_agents_from_session_history(
            &mut team,
            &parent,
            &[tool("read_file")],
            root.path(),
        );
        assert_eq!(restored, 0);
        assert!(team.registry.find_by_role("dev").is_none());
        assert_eq!(
            explicitly_terminated_agent_ids(&parent),
            std::collections::HashSet::from(["agent-dev".to_string()])
        );
    }

    #[test]
    fn restore_status_with_id_does_not_terminate_same_label_agent() {
        let root = isolated_storage_root();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] Worker (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] Worker (test) 已加入团队 id=agent-test".to_string(),
        );
        parent.append_message(
            tiangong_core::session::MessageRole::System,
            "[Agent] Worker 状态变更: terminated id=agent-dev".to_string(),
        );

        let restored = restore_agents_from_session_history(
            &mut team,
            &parent,
            &[tool("read_file")],
            root.path(),
        );
        assert_eq!(restored, 1);
        assert!(team.registry.find_by_role("dev").is_none());
        assert_eq!(
            team.registry.find_by_role("test").unwrap().agent_id,
            "agent-test"
        );
    }

    #[test]
    fn persisted_agent_descriptor_restores_prompt_and_tool_scope_exactly() {
        let root = isolated_storage_root();
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        let tools = [tool("read_file"), tool("write_file")];
        let result = execute_team_tool(
            &mut team,
            "main",
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "只做只读审计",
                    "tools": ["read_file"]
                }),
            ),
            &mut parent,
            &tools,
            &tx,
        );
        assert!(result.ok);

        let json = serde_json::to_string(&parent).unwrap();
        let restored_parent: Session = serde_json::from_str(&json).unwrap();
        let mut restored_team = TeamContext::new();
        assert_eq!(
            restore_agents_from_session_history(
                &mut restored_team,
                &restored_parent,
                &tools,
                root.path(),
            ),
            1
        );
        let restored = restored_team.registry.find_by_role("dev").unwrap();
        assert_eq!(restored.system_prompt, "只做只读审计");
        assert_eq!(restored.tools, vec!["read_file"]);
    }

    #[test]
    fn user_mentions_route_directly_to_target_agent() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "agent"
                }),
            ),
            &parent,
            &[tool("read_file")],
            &tx,
        );
        assert!(result.ok);

        assert!(route_user_mentions(&mut team, "@dev 修改 src/main.rs", &tx));
        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let inbox = team.registry.drain_inbox(&dev_id);
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from, "user");
        assert_eq!(inbox[0].content, "修改 src/main.rs");
    }

    #[test]
    fn pending_user_delivery_restores_idempotently_with_ready_content() {
        let mut team = TeamContext::new();
        team.registry.register_with_session(
            AgentDescriptor {
                agent_id: "agent-dev".to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "agent".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            },
            Session::new("child"),
        );
        let prepared = vec![
            ContentBlock::text("@dev 检查附件"),
            ContentBlock::model_instruction("保持块顺序"),
        ];
        let mut parent = Session::new("parent");
        let deliveries = plan_user_mention_deliveries(&team, "source-1", &prepared);
        parent.pending_plugin_deliveries = deliveries;

        assert_eq!(restore_pending_agent_deliveries(&mut team, &parent), 1);
        assert_eq!(restore_pending_agent_deliveries(&mut team, &parent), 1);
        let inbox = team.registry.drain_inbox("agent-dev");
        assert_eq!(inbox.len(), 1);
        assert!(inbox[0].message.id.starts_with("source-1:agent-dev:"));
        assert_eq!(
            inbox[0].additional_content,
            vec![
                ContentBlock::text("检查附件"),
                ContentBlock::model_instruction("保持块顺序"),
            ]
        );
    }

    #[test]
    fn prepared_mention_waits_for_a_separate_run_when_agent_is_active() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        let result = execute_create_agent(
            &mut team,
            &call(
                "create_agent",
                json!({
                    "role": "dev",
                    "label": "Developer",
                    "system_prompt": "agent"
                }),
            ),
            &parent,
            &[tool("read_file")],
            &tx,
        );
        assert!(result.ok);
        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        team.register_active_agent(
            dev_id.clone(),
            command_tx,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
        );

        let asset = tiangong_types::StoredAsset {
            asset_id: "asset-1".to_string(),
            local_path: "/tmp/report.pdf".to_string(),
            original_name: "report.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 12,
            kind: tiangong_types::MediaKind::File,
        };
        let expected_additional = vec![
            ContentBlock::asset_reference(asset),
            ContentBlock::model_instruction("使用 message_id=source-message、attachment_index=0"),
        ];
        let mut content = vec![ContentBlock::text("@dev 检查附件")];
        content.extend(expected_additional.clone());

        assert!(route_user_mentions_with_content(
            &mut team,
            Some("source-message"),
            content,
            &tx,
        ));

        assert!(command_rx.try_recv().is_err());
        let entry = team
            .registry
            .drain_inbox(&dev_id)
            .pop()
            .expect("direct user message should wait in the persistent inbox");
        assert_eq!(entry.session_message_id.as_deref(), Some("source-message"));
        let prepared = prepared_agent_message_for_prompt(&entry.message, entry.additional_content);
        assert!(content_blocks_text(&prepared).contains("检查附件"));
        assert_eq!(&prepared[1..], expected_additional.as_slice());
    }

    #[test]
    fn prepared_mention_waits_in_idle_inbox_without_losing_content() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let parent = Session::new("parent");
        assert!(
            execute_create_agent(
                &mut team,
                &call(
                    "create_agent",
                    json!({
                        "role": "dev",
                        "label": "Developer",
                        "system_prompt": "agent"
                    }),
                ),
                &parent,
                &[tool("read_file")],
                &tx,
            )
            .ok
        );
        let dev_id = team.registry.find_by_role("dev").unwrap().agent_id.clone();
        let instruction =
            ContentBlock::model_instruction("使用 message_id=source-message、attachment_index=0");

        assert!(route_user_mentions_with_content(
            &mut team,
            Some("source-message"),
            vec![ContentBlock::text("@dev 检查附件"), instruction.clone(),],
            &tx,
        ));

        let entry = team
            .registry
            .drain_inbox(&dev_id)
            .pop()
            .expect("idle agent should retain the routed message");
        assert_eq!(entry.session_message_id.as_deref(), Some("source-message"));
        assert!(matches!(
            &entry.additional_content[0],
            ContentBlock::Text { text } if text == "检查附件"
        ));
        assert_eq!(entry.additional_content[1], instruction);
        let prepared = prepared_agent_message_for_prompt(&entry.message, entry.additional_content);
        assert!(content_blocks_text(&prepared).contains("检查附件"));
        assert!(matches!(
            &prepared[1],
            ContentBlock::ModelInstruction { text }
                if text.contains("message_id=source-message")
        ));
    }

    #[test]
    fn mention_parser_supports_multiple_roles_and_all() {
        let route = parse_agent_mention_route("@dev @test 检查这个改动").unwrap();
        assert_eq!(route.roles, vec!["dev", "test"]);
        assert!(!route.broadcast);
        assert_eq!(route.content, "检查这个改动");

        let route = parse_agent_mention_route("@all 同步状态").unwrap();
        assert!(route.broadcast);
        assert_eq!(route.content, "同步状态");
    }

    #[test]
    fn routed_message_preserves_interleaved_text_and_images() {
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        team.registry.register_with_session(
            AgentDescriptor {
                agent_id: "dev-agent".to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "agent".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            },
            Session::new("child"),
        );
        let image = |asset_id: &str| ContentBlock::Image {
            asset: tiangong_types::StoredAsset {
                asset_id: asset_id.to_string(),
                local_path: format!("/tmp/{asset_id}.png"),
                original_name: format!("{asset_id}.png"),
                mime_type: "image/png".to_string(),
                size: 1,
                kind: tiangong_types::MediaKind::Image,
            },
            data: None,
        };

        assert!(route_user_mentions_with_content(
            &mut team,
            Some("source-message"),
            vec![
                ContentBlock::text("@dev 比较"),
                image("one"),
                ContentBlock::text("第一张"),
                image("two"),
                ContentBlock::text("第二张"),
            ],
            &tx,
        ));

        let entry = team.registry.drain_inbox("dev-agent").remove(0);
        let prepared = prepared_agent_message_for_prompt(&entry.message, entry.additional_content);
        assert!(matches!(
            &prepared[0],
            ContentBlock::Text { text } if text.ends_with("比较")
        ));
        assert!(
            matches!(&prepared[1], ContentBlock::Image { asset, .. } if asset.asset_id == "one")
        );
        assert!(matches!(&prepared[2], ContentBlock::Text { text } if text == "第一张"));
        assert!(
            matches!(&prepared[3], ContentBlock::Image { asset, .. } if asset.asset_id == "two")
        );
        assert!(matches!(&prepared[4], ContentBlock::Text { text } if text == "第二张"));
    }
}
