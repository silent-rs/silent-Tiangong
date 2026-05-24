//! Sub Agent 生命周期管理

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;

use crate::agent_team::descriptor::{AgentDescriptor, AgentStatus};
use crate::agent_team::file_lock::FileLockManager;
use crate::agent_team::message_bus::AgentMessage;
use crate::agent_team::registry::AgentRegistry;
use crate::agent_team::tools::MAX_AGENTS;
use crate::core::command::Command;
use crate::model::{ToolCall, ToolSpec};
use crate::session::{Session, now_text};
use crate::tool::ToolResult;
use tiangong_types::StreamEvent;
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
        media: Vec<tiangong_types::MediaAsset>,
    ) {
        let dispatched = if let Some(tx) = self.active_agent_senders.get(agent_id) {
            tx.send(Command::Message {
                content: format_agent_message_for_prompt(&message),
                message_id: Some(message.id.clone()),
                media,
            })
            .is_ok()
        } else {
            false
        };

        if !dispatched {
            self.registry.deliver_message(agent_id, message);
            if let Some(waker) = &self.dispatch_waker {
                let _ = waker.send(());
            }
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

fn format_agent_message_for_prompt(message: &AgentMessage) -> String {
    format!(
        "[from:{} at {}]\n{}",
        message.from, message.created_at, message.content
    )
}

#[derive(Debug, Clone)]
struct RestoredAgent {
    agent_id: String,
    role: String,
    label: String,
    status: AgentStatus,
}

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

        if let Some(agent) = parse_agent_created_message(&message.text_content()) {
            restored.retain(|existing| existing.role != agent.role);
            restored.push(agent);
            continue;
        }

        if let Some((label, status, agent_id)) = parse_agent_status_message(&message.text_content())
        {
            for agent in &mut restored {
                let id_matches = agent_id.as_deref() == Some(agent.agent_id.as_str());
                if id_matches || agent.label == label {
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
            system_prompt: format!(
                "你是从会话历史恢复的子 Agent，角色为 {}（{}）。请延续当前会话上下文，按该角色职责处理用户和主 Agent 分配的任务。",
                agent.label, agent.role
            ),
            tools: tool_names.clone(),
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

    team.dispatch_agent_message(&target.agent_id, message, Vec::new());

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
        team.dispatch_agent_message(agent_id, msg, Vec::new());
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
    parent_session: &Session,
    parent_tools: &[ToolSpec],
    stream_tx: &StdSender<StreamEvent>,
) -> ToolResult {
    match call.name.as_str() {
        "create_agent" => execute_create_agent(team, call, parent_session, parent_tools, stream_tx),
        "dismiss_agent" => execute_dismiss_agent(team, call, stream_tx),
        "send_message" => execute_send_message(team, current_agent_id, call, stream_tx),
        "broadcast_message" => execute_broadcast_message(team, current_agent_id, call, stream_tx),
        "notify_user" => execute_notify_user(team, current_agent_id, call, stream_tx),
        "lock_file" => execute_lock_file(team, current_agent_id, call, stream_tx),
        "unlock_file" => execute_unlock_file(team, current_agent_id, call, stream_tx),
        _ => error_tool_result(&call.name, &format!("未知团队工具: {}", call.name)),
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
    route_user_mentions_with_media(team, content, Vec::new(), stream_tx)
}

/// 将用户 @提及消息投递给目标 Agent。运行中的 Agent 会实时注入当前循环，
/// 空闲 Agent 会进入收件箱并唤醒调度器启动新一轮执行。
pub fn route_user_mentions_with_media(
    team: &mut TeamContext,
    content: &str,
    media: Vec<tiangong_types::MediaAsset>,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    let Some(route) = parse_agent_mention_route(content) else {
        return false;
    };
    if route.content.trim().is_empty() {
        return false;
    }

    let targets: Vec<_> = if route.broadcast {
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

    if targets.is_empty() {
        return false;
    }

    for (agent_id, _role, label) in targets {
        let message = AgentMessage {
            id: scru128::new().to_string(),
            from: "user".to_string(),
            to: agent_id.clone(),
            content: route.content.clone(),
            priority: crate::agent_team::message_bus::MessagePriority::Normal,
            created_at: now_text(),
        };
        team.dispatch_agent_message(&agent_id, message, media.clone());
        let _ = stream_tx.send(StreamEvent::AgentMessage {
            from_agent_id: "user".to_string(),
            from_agent_label: "User".to_string(),
            to_agent_id: agent_id,
            to_agent_label: label,
            content: route.content.clone(),
        });
    }

    true
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
pub fn persist_child_session(parent_session: &Session, agent_id: &str, session: &Session) {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let agents_dir = home
        .join(".tiangong")
        .join("sessions")
        .join(&parent_session.id)
        .join("agents");
    if std::fs::create_dir_all(&agents_dir).is_err() {
        tracing::warn!("创建 agents 目录失败");
        return;
    }
    let path = agents_dir.join(format!("{agent_id}.json"));
    match serde_json::to_string_pretty(session) {
        Ok(content) => {
            if let Err(err) = std::fs::write(&path, content) {
                tracing::warn!(error = %err, "child session 持久化写入失败");
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "child session 序列化失败");
        }
    }
}

/// 从磁盘加载 child_session
pub fn load_child_session(parent_session: &Session, agent_id: &str) -> Option<Session> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = home
        .join(".tiangong")
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
    fn mention_parser_supports_multiple_roles_and_all() {
        let route = parse_agent_mention_route("@dev @test 检查这个改动").unwrap();
        assert_eq!(route.roles, vec!["dev", "test"]);
        assert!(!route.broadcast);
        assert_eq!(route.content, "检查这个改动");

        let route = parse_agent_mention_route("@all 同步状态").unwrap();
        assert!(route.broadcast);
        assert_eq!(route.content, "同步状态");
    }
}
