//! Sub Agent 生命周期管理

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;

use crate::agent_team::descriptor::{AgentDescriptor, AgentStatus};
use crate::agent_team::file_lock::FileLockManager;
use crate::agent_team::message_bus::{AgentInboxEntry, AgentMessage};
use crate::agent_team::registry::AgentRegistry;
use crate::agent_team::tools::MAX_AGENTS;
use crate::core::command::Command;
use crate::model::{ToolCall, ToolSpec};
use crate::session::{PendingAgentDelivery, Session, now_text};
use crate::tool::ToolResult;
use tiangong_types::{ContentBlock, PreparedUserMessage, StreamEvent};
use tokio::sync::mpsc as tokio_mpsc;

/// 团队执行上下文，随 ReactEngine 的 execute_turn 传递
pub struct TeamContext {
    /// Agent 注册表
    pub registry: AgentRegistry,
    /// 文件锁管理器
    pub file_locks: FileLockManager,
    /// 发给主 Agent 的消息
    pub main_inbox: Vec<AgentMessage>,
    /// 正在运行的 Agent 实时命令通道
    active_agent_senders: HashMap<String, tokio_mpsc::UnboundedSender<Command>>,
    /// 空闲 Agent 收到新消息时唤醒调度器
    dispatch_waker: Option<tokio_mpsc::UnboundedSender<()>>,
}

impl TeamContext {
    pub fn new() -> Self {
        Self {
            registry: AgentRegistry::new(),
            file_locks: FileLockManager::new(),
            main_inbox: Vec::new(),
            active_agent_senders: HashMap::new(),
            dispatch_waker: None,
        }
    }

    pub fn deliver_main_message(&mut self, message: AgentMessage) {
        self.main_inbox.push(message);
    }

    pub fn drain_main_inbox(&mut self) -> Vec<AgentMessage> {
        std::mem::take(&mut self.main_inbox)
    }

    pub(crate) fn register_active_agent(
        &mut self,
        agent_id: String,
        tx: tokio_mpsc::UnboundedSender<Command>,
    ) {
        self.active_agent_senders.insert(agent_id, tx);
    }

    pub(crate) fn unregister_active_agent(&mut self, agent_id: &str) {
        self.active_agent_senders.remove(agent_id);
    }

    pub(crate) fn set_dispatch_waker(&mut self, tx: tokio_mpsc::UnboundedSender<()>) {
        self.dispatch_waker = Some(tx);
    }

    pub(crate) fn clear_dispatch_waker(&mut self) {
        self.dispatch_waker = None;
    }

    pub(crate) fn dispatch_agent_message(
        &mut self,
        agent_id: &str,
        message: AgentMessage,
        additional_content: Vec<ContentBlock>,
        session_message_id: Option<String>,
    ) {
        // Agent 输入按消息来源分批串行执行；运行中追加会让 direct-user 与
        // delegated 结果共享一次总结，无法可靠决定结果应展示还是回灌 Main。
        self.queue_agent_message(agent_id, message, additional_content, session_message_id);
    }

    pub(crate) fn queue_agent_message(
        &mut self,
        agent_id: &str,
        message: AgentMessage,
        additional_content: Vec<ContentBlock>,
        session_message_id: Option<String>,
    ) {
        self.registry.deliver_inbox_entry(
            agent_id,
            AgentInboxEntry {
                message,
                additional_content,
                session_message_id,
            },
        );
        if let Some(waker) = &self.dispatch_waker {
            let _ = waker.send(());
        }
    }
}

impl Default for TeamContext {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_agent_role(role: &str) -> String {
    role.trim().trim_start_matches('@').to_string()
}

pub(crate) fn prepared_agent_message_for_prompt(
    message: &AgentMessage,
    mut additional_content: Vec<ContentBlock>,
) -> PreparedUserMessage {
    let source_prefix = format!("[from:{} at {}]\n", message.from, message.created_at);
    if additional_content.is_empty() {
        return PreparedUserMessage::text(format!("{source_prefix}{}", message.content));
    }

    if let Some(ContentBlock::Text { text }) = additional_content
        .iter_mut()
        .find(|block| matches!(block, ContentBlock::Text { .. }))
    {
        text.insert_str(0, &source_prefix);
    } else {
        additional_content.insert(0, ContentBlock::text(source_prefix));
    }
    PreparedUserMessage::new(additional_content)
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
) -> usize {
    let mut restored: Vec<RestoredAgent> = Vec::new();

    for message in &parent_session.messages {
        if message.role != crate::session::MessageRole::System {
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
        let child_session = load_child_session(parent_session, &agent.agent_id)
            .unwrap_or_else(|| create_child_session(parent_session, &agent.label));
        team.registry
            .register_with_session(descriptor, child_session);
        count += 1;
    }

    count
}

fn parse_persisted_agent_descriptor(message: &crate::session::Message) -> Option<RestoredAgent> {
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

    let child_session = create_child_session(parent_session, &label);

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
        team.dispatch_agent_message(
            &target_id,
            AgentMessage {
                id: scru128::new().to_string(),
                from: "system".to_string(),
                to: target_id.clone(),
                content: notification.clone(),
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
                created_at: now_text(),
            },
            Vec::new(),
            None,
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
        execution: Some(crate::tool::ToolExecutionRecord {
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
        execution: Some(crate::tool::ToolExecutionRecord {
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
            id: scru128::new().to_string(),
            from: current_agent_id.to_string(),
            to: "main".to_string(),
            content: content.clone(),
            priority: crate::agent_team::message_bus::MessagePriority::Normal,
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
            execution: Some(crate::tool::ToolExecutionRecord {
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
        id: scru128::new().to_string(),
        from: current_agent_id.to_string(),
        to: target.agent_id.clone(),
        content: content.clone(),
        priority: crate::agent_team::message_bus::MessagePriority::Normal,
        created_at: now_text(),
    };

    team.dispatch_agent_message(&target.agent_id, message, Vec::new(), None);

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
        execution: Some(crate::tool::ToolExecutionRecord {
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

    let message = AgentMessage {
        id: scru128::new().to_string(),
        from: current_agent_id.to_string(),
        to: "all".to_string(),
        content: content.clone(),
        priority: crate::agent_team::message_bus::MessagePriority::Normal,
        created_at: now_text(),
    };

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
            to: agent_id.clone(),
            ..message.clone()
        };
        team.dispatch_agent_message(agent_id, msg, Vec::new(), None);
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
        execution: Some(crate::tool::ToolExecutionRecord {
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
        execution: Some(crate::tool::ToolExecutionRecord {
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
                execution: Some(crate::tool::ToolExecutionRecord {
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
            execution: Some(crate::tool::ToolExecutionRecord {
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
                execution: Some(crate::tool::ToolExecutionRecord {
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
            execution: Some(crate::tool::ToolExecutionRecord {
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

/// 判断是否为团队协作工具
pub fn is_team_tool(name: &str) -> bool {
    matches!(
        name,
        "create_agent"
            | "dismiss_agent"
            | "send_message"
            | "broadcast_message"
            | "notify_user"
            | "lock_file"
            | "unlock_file"
    )
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
            if result.ok
                && let Some(role) = call.arguments.get("role").and_then(|value| value.as_str())
                && let Some(agent) = team.registry.find_by_role(role)
            {
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
            if result.ok
                && let Some((agent_id, label)) = previous
            {
                append_team_history_message(
                    parent_session,
                    stream_tx,
                    format!("[Agent] {label} 状态变更: terminated id={agent_id}"),
                    None,
                );
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
    let mut message = crate::session::Message::new(crate::session::MessageRole::System, content);
    message.model_excluded = true;
    if let Some(descriptor) = descriptor
        && let Ok(json) = serde_json::to_string(descriptor)
    {
        message
            .content
            .push(ContentBlock::model_instruction(format!(
                "{AGENT_DESCRIPTOR_MARKER}{json}"
            )));
    }
    let message_id = message.id.clone();
    session.messages.push(message);
    if let Err(error) = session.try_persist_to_disk() {
        tracing::warn!(%error, "持久化 Agent 生命周期记录失败");
    }
    crate::react::message::emit_session_message_upsert(session, stream_tx, &message_id);
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
    route_user_mentions_with_content(team, None, PreparedUserMessage::text(content), stream_tx)
}

/// 在用户消息持久化前生成确定性的直达投递计划。
pub(crate) fn plan_user_mention_deliveries(
    team: &TeamContext,
    source_message_id: &str,
    prepared: &PreparedUserMessage,
) -> Vec<PendingAgentDelivery> {
    build_user_mention_entries(team, Some(source_message_id), prepared.clone())
        .into_iter()
        .map(|(target_agent_id, _label, entry)| PendingAgentDelivery {
            delivery_id: entry.message.id,
            source_message_id: source_message_id.to_string(),
            target_agent_id,
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
        .pending_agent_deliveries
        .iter()
        .filter(|delivery| delivery.source_message_id == source_message_id)
        .cloned()
        .collect::<Vec<_>>();
    let mut dispatched = false;
    for delivery in deliveries {
        let Some(agent) = team.registry.get(&delivery.target_agent_id) else {
            continue;
        };
        if agent.status == AgentStatus::Terminated {
            continue;
        }
        let label = agent.label.clone();
        let content = delivery.content.clone();
        team.queue_agent_message(
            &delivery.target_agent_id,
            AgentMessage {
                id: delivery.delivery_id,
                from: "user".to_string(),
                to: delivery.target_agent_id.clone(),
                content: delivery.content,
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
                created_at: delivery.created_at,
            },
            delivery.additional_content,
            Some(delivery.source_message_id),
        );
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: "user".to_string(),
            from_agent_label: "User".to_string(),
            to_agent_id: delivery.target_agent_id,
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
    for delivery in &parent_session.pending_agent_deliveries {
        let Some(agent) = team.registry.get(&delivery.target_agent_id) else {
            continue;
        };
        if agent.status == AgentStatus::Terminated {
            continue;
        }
        team.queue_agent_message(
            &delivery.target_agent_id,
            AgentMessage {
                id: delivery.delivery_id.clone(),
                from: "user".to_string(),
                to: delivery.target_agent_id.clone(),
                content: delivery.content.clone(),
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
                created_at: delivery.created_at.clone(),
            },
            delivery.additional_content.clone(),
            Some(delivery.source_message_id.clone()),
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
    prepared: PreparedUserMessage,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    let entries = build_user_mention_entries(team, source_message_id, prepared);
    if entries.is_empty() {
        return false;
    }
    for (agent_id, label, entry) in entries {
        let content = entry.message.content.clone();
        team.queue_agent_message(
            &agent_id,
            entry.message,
            entry.additional_content,
            entry.session_message_id,
        );
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
    prepared: PreparedUserMessage,
) -> Vec<(String, String, AgentInboxEntry)> {
    let content = prepared.text_content();
    let Some(route) = parse_agent_mention_route(&content) else {
        return Vec::new();
    };
    if route.content.trim().is_empty() {
        return Vec::new();
    }

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
    let mut routed_content = prepared.content;
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

    let routed_content = PreparedUserMessage::new(routed_content).stable().content;
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
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
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

pub(crate) fn error_tool_result(name: &str, message: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: format!("{name} 失败: {message}"),
        stdout: String::new(),
        stderr: message.to_string(),
        exit_code: 1,
        execution: Some(crate::tool::ToolExecutionRecord {
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
fn create_child_session(parent: &Session, title: &str) -> Session {
    let mut child = Session::new(title);
    child.cwd = parent.cwd.clone();
    child.reasoning_effort = parent.reasoning_effort.clone();
    child.parent_session_id = Some(parent.id.clone());
    child
}

/// 持久化 child_session 到磁盘
pub fn persist_child_session(
    parent_session: &Session,
    agent_id: &str,
    session: &Session,
) -> Result<(), String> {
    let agents_dir = crate::storage::storage_root()
        .join("sessions")
        .join(&parent_session.id)
        .join("agents");
    std::fs::create_dir_all(&agents_dir)
        .map_err(|error| format!("创建 agents 目录失败：{error}"))?;
    let path = agents_dir.join(format!("{agent_id}.json"));
    let content = serde_json::to_string_pretty(session)
        .map_err(|error| format!("child session 序列化失败：{error}"))?;
    crate::session::atomic_replace_file(&path, content.as_bytes())
        .map_err(|error| format!("child session 持久化写入失败：{error}"))
}

/// 从磁盘加载 child_session
pub fn load_child_session(parent_session: &Session, agent_id: &str) -> Option<Session> {
    let path = crate::storage::storage_root()
        .join("sessions")
        .join(&parent_session.id)
        .join("agents")
        .join(format!("{agent_id}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use serde_json::json;

    use super::*;

    /// 把 storage_root 注入为临时目录，供触及持久化的用例使用。
    /// 返回的 TempDir 须保持存活到用例结束。
    fn isolated_storage_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        crate::storage::set_storage_root(dir.path().to_path_buf());
        dir
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

        let result = execute_broadcast_message(
            &mut team,
            &dev_id,
            &call(
                "broadcast_message",
                json!({
                    "content": "sync status",
                    "exclude": ["@dev"]
                }),
            ),
            &tx,
        );

        assert!(result.ok);
        assert!(team.registry.drain_inbox(&dev_id).is_empty());
        assert_eq!(team.registry.drain_inbox(&test_id).len(), 1);
        assert_eq!(team.registry.drain_inbox(&pm_id).len(), 1);
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

        assert!(
            team.file_locks
                .ensure_can_write(&path, "dev-agent", &now)
                .is_err()
        );

        let result = execute_lock_file(
            &mut team,
            "dev-agent",
            &call("lock_file", json!({"path": "src/auth.rs"})),
            &tx,
        );
        assert!(result.ok);
        assert!(
            team.file_locks
                .ensure_can_write(&path, "dev-agent", &now)
                .is_ok()
        );
        assert!(
            team.file_locks
                .ensure_can_write(&path, "test-agent", &now)
                .is_err()
        );
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
    fn restored_agent_from_session_history_can_be_dismissed() {
        let _root = isolated_storage_root();
        let (tx, _rx) = mpsc::channel();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] 开发者 (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            crate::session::MessageRole::System,
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
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] 开发者 (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] 开发者 状态变更: terminated id=agent-dev".to_string(),
        );

        let restored =
            restore_agents_from_session_history(&mut team, &parent, &[tool("read_file")]);
        assert_eq!(restored, 0);
        assert!(team.registry.find_by_role("dev").is_none());
    }

    #[test]
    fn restore_status_with_id_does_not_terminate_same_label_agent() {
        let _root = isolated_storage_root();
        let mut team = TeamContext::new();
        let mut parent = Session::new("parent");
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] Worker (dev) 已加入团队 id=agent-dev".to_string(),
        );
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] Worker (test) 已加入团队 id=agent-test".to_string(),
        );
        parent.append_message(
            crate::session::MessageRole::System,
            "[Agent] Worker 状态变更: terminated id=agent-dev".to_string(),
        );

        let restored =
            restore_agents_from_session_history(&mut team, &parent, &[tool("read_file")]);
        assert_eq!(restored, 1);
        assert!(team.registry.find_by_role("dev").is_none());
        assert_eq!(
            team.registry.find_by_role("test").unwrap().agent_id,
            "agent-test"
        );
    }

    #[test]
    fn persisted_agent_descriptor_restores_prompt_and_tool_scope_exactly() {
        let _root = isolated_storage_root();
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
            restore_agents_from_session_history(&mut restored_team, &restored_parent, &tools),
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
        let prepared = PreparedUserMessage::new(vec![
            ContentBlock::text("@dev 检查附件"),
            ContentBlock::model_instruction("保持块顺序"),
        ]);
        let mut parent = Session::new("parent");
        let deliveries = plan_user_mention_deliveries(&team, "source-1", &prepared);
        parent.replace_pending_agent_deliveries("source-1", deliveries);

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
        team.register_active_agent(dev_id.clone(), command_tx);

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
            PreparedUserMessage::new(content),
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
        assert!(prepared.text_content().contains("检查附件"));
        assert_eq!(&prepared.content[1..], expected_additional.as_slice());
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
            PreparedUserMessage::new(vec![
                ContentBlock::text("@dev 检查附件"),
                instruction.clone(),
            ]),
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
        assert!(prepared.text_content().contains("检查附件"));
        assert!(matches!(
            &prepared.content[1],
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
            PreparedUserMessage::new(vec![
                ContentBlock::text("@dev 比较"),
                image("one"),
                ContentBlock::text("第一张"),
                image("two"),
                ContentBlock::text("第二张"),
            ]),
            &tx,
        ));

        let entry = team.registry.drain_inbox("dev-agent").remove(0);
        let prepared = prepared_agent_message_for_prompt(&entry.message, entry.additional_content);
        assert!(matches!(
            &prepared.content[0],
            ContentBlock::Text { text } if text.ends_with("比较")
        ));
        assert!(
            matches!(&prepared.content[1], ContentBlock::Image { asset, .. } if asset.asset_id == "one")
        );
        assert!(matches!(&prepared.content[2], ContentBlock::Text { text } if text == "第一张"));
        assert!(
            matches!(&prepared.content[3], ContentBlock::Image { asset, .. } if asset.asset_id == "two")
        );
        assert!(matches!(&prepared.content[4], ContentBlock::Text { text } if text == "第二张"));
    }
}
