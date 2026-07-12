//! 7 个团队工具的规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `create_agent` /
//! `dismiss_agent` / `send_message` / `broadcast_message` / `notify_user` /
//! `lock_file` / `unlock_file` 七个工具。工具规格迁自 core `agent_team/tools.rs`；
//! handler 逻辑迁自 `agent_team/lifecycle.rs`（见 [`crate::lifecycle`]）。
//!
//! 与原 core 实现的差异：
//! - 工具经统一 `tool_overrides` 分发（原 core 在 engine 里有独立的 `is_team_tool`
//!   拦截分支，现已删除）。
//! - handler 经 `&self` 访问插件持有的 `TeamContext`（`Arc<Mutex<...>>`），而非
//!   engine 传入的 `team`。
//! - 流事件经 `PluginFeedbackTx` 投递（原 core 直接 `stream_tx.send`）。为复用原
//!   lifecycle 的 `&StdSender<StreamEvent>` 签名，handler 内同步排空桥接 channel。
//! - 调用方身份由 engine 显式传入，不从 Session 展示状态推断。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tiangong_core::core::plugin::{PluginFeedback, PluginFeedbackTx};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::StreamEvent;

use crate::lifecycle::execute_team_tool;
use crate::plugin::{AgentTeamPlugin, MAIN_AGENT_ID};
use crate::state::AgentStatus;
use crate::TeamContext;

impl ToolSpecProvider for AgentTeamPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "create_agent".to_string(),
                description: "创建一个 Sub Agent 加入团队。Agent 拥有独立的执行上下文和指定角色，持续存在直到被解散。"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "role": {
                            "type": "string",
                            "description": "Agent 角色标识，用于 @提及（如 'pm'、'dev'、'test'）"
                        },
                        "label": {
                            "type": "string",
                            "description": "Agent 显示名称（如 'Project Manager'、'Developer'）"
                        },
                        "system_prompt": {
                            "type": "string",
                            "description": "Agent 的角色系统提示，定义其职责和行为规范"
                        },
                        "tools": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Agent 可用的工具列表。不指定时继承你的全部工具（不含 create_agent/dismiss_agent）。建议根据任务需要精确授权。"
                        }
                    },
                    "required": ["role", "label", "system_prompt"]
                }),
            },
            ToolSpec {
                name: "dismiss_agent".to_string(),
                description: "解散指定的 Sub Agent，释放其持有的所有资源（文件锁等）。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "role": {
                            "type": "string",
                            "description": "要解散的 Agent 角色"
                        }
                    },
                    "required": ["role"]
                }),
            },
            ToolSpec {
                name: "send_message".to_string(),
                description: "向指定 Agent 发送消息。支持 @role 格式指定目标。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "to": {
                            "type": "string",
                            "description": "目标 Agent 的 role"
                        },
                        "content": {
                            "type": "string",
                            "description": "消息内容"
                        },
                        "priority": {
                            "type": "string",
                            "enum": ["normal", "urgent"],
                            "description": "消息优先级，默认 normal"
                        }
                    },
                    "required": ["to", "content"]
                }),
            },
            ToolSpec {
                name: "broadcast_message".to_string(),
                description: "向所有存活 Agent 广播消息。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "广播内容"
                        },
                        "exclude": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "排除的 Agent role 列表（通常排除自己）"
                        }
                    },
                    "required": ["content"]
                }),
            },
            ToolSpec {
                name: "notify_user".to_string(),
                description: "直接向用户推送消息，无需经主 Agent 转发。用于进度汇报、阻塞通知、提问等场景。推送消息会携带 Agent 标识。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "推送给用户的内容"
                        },
                        "level": {
                            "type": "string",
                            "enum": ["info", "warning", "error", "question"],
                            "description": "消息级别，默认 info"
                        }
                    },
                    "required": ["content"]
                }),
            },
            ToolSpec {
                name: "lock_file".to_string(),
                description: "获取文件编辑锁。编辑文件前必须先获取锁，防止多 Agent 冲突。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "要锁定的文件路径"
                        }
                    },
                    "required": ["path"]
                }),
            },
            ToolSpec {
                name: "unlock_file".to_string(),
                description: "释放文件编辑锁。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "要释放的文件路径"
                        }
                    },
                    "required": ["path"]
                }),
            },
        ]
    }
}

impl ToolOverrideHandler for AgentTeamPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // 桥接：lifecycle handler 发出的 StreamEvent 先入本地 mpsc channel（保持原
        // &StdSender 签名，便于单测），执行完后同步 drain 并经 feedback 投递。
        // 不再用独立转发线程（避免 thread::spawn + join 在 async 上下文中的时序问题）。
        let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
        let feedback_tx = self.feedback_tx().map(PluginFeedbackTx::for_current_turn);

        // 调用方身份由 ReactEngine 显式传入，不从 Session 展示状态推断。
        let current_agent_id = actor_id.to_string();
        let parent_tools = self.parent_tools_snapshot();
        let delivery_protocol_store = self.delivery_protocol_store();
        let delivery_protocol_session_id = self
            .prompt_config_snapshot()
            .map(|config| config.session_id.clone())
            .or_else(|| session.parent_session_id.clone())
            .or_else(|| Some(session.id.clone()));
        let normalized_call = match self.normalize_team_tool_call(call) {
            Ok(call) => call,
            Err(error) => {
                let result = crate::lifecycle::error_tool_result(&call.name, &error);
                return Box::pin(async move { Some(result) });
            }
        };

        // 锁 team，执行工具（投递消息 / 注册 Agent / 锁文件等，同步部分）。
        let (sync_result, dismissed_delivery_ids, discard_stream_events) = {
            let Ok(mut team) = self.team.lock() else {
                let err = crate::lifecycle::error_tool_result(&call.name, "团队状态锁定失败");
                return Box::pin(async move { Some(err) });
            };
            // Cancel / Dismiss 与工具副作用共享 TeamContext 锁作为线性化边界。
            // 非 Main 调用方必须仍存活、仍持有当前执行句柄且尚未进入取消/关闭终态。
            if current_agent_id != MAIN_AGENT_ID {
                let actor_alive = team
                    .registry
                    .get(&current_agent_id)
                    .is_some_and(|actor| actor.status != AgentStatus::Terminated);
                let active_handle = team.active_agent_handle(&current_agent_id);
                let actor_can_continue = active_handle.as_ref().is_some_and(|handle| {
                    !handle
                        .cancel_flag
                        .load(std::sync::atomic::Ordering::Acquire)
                        && !handle
                            .shutdown_flag
                            .load(std::sync::atomic::Ordering::Acquire)
                });
                if !actor_alive || !actor_can_continue {
                    let err = crate::lifecycle::error_tool_result(
                        &normalized_call.name,
                        "当前 Sub Agent 已取消、关闭或不再存活，团队工具调用未执行",
                    );
                    return Box::pin(async move { Some(err) });
                }
            }

            let inbox_snapshot = matches!(
                normalized_call.name.as_str(),
                "send_message" | "broadcast_message"
            )
            .then(|| team.registry.inbox_snapshot());
            // 解散属于显式取消：在同一个 team 临界区内先快照并持久化取消记录，
            // 成功后才允许 unregister 与 active flag 置位。持久化失败时团队状态原样
            // 保留，不能先丢收件箱再尝试补 tombstone。
            let dismiss_context = (normalized_call.name == "dismiss_agent")
                .then(|| {
                    normalized_call
                        .arguments
                        .get("role")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|role| team.registry.find_by_role(role))
                        .filter(|agent| !crate::lifecycle::agent_is_running(&team, agent))
                        .map(|agent| agent.agent_id.clone())
                })
                .flatten()
                .map(|agent_id| {
                    let handle = team.active_agent_handle(&agent_id);
                    let mut delivery_ids = team.registry.pending_delivery_ids_for(&agent_id);
                    if let Some(delivery_id) = handle
                        .as_ref()
                        .and_then(|handle| handle.pending_delivery_id.clone())
                    {
                        delivery_ids.push(delivery_id);
                    }
                    (handle, delivery_ids)
                });
            let cancellation_error = dismiss_context.as_ref().and_then(|(_, delivery_ids)| {
                self.persist_delivery_cancellation(delivery_ids).err()
            });
            if let Some(error) = cancellation_error {
                (
                    crate::lifecycle::error_tool_result(
                        "dismiss_agent",
                        &format!("取消记录保存失败，Agent 未解散：{error}"),
                    ),
                    None,
                    false,
                )
            } else {
                let mut result = execute_team_tool(
                    &mut team,
                    &current_agent_id,
                    &normalized_call,
                    session,
                    &parent_tools,
                    &stream_tx,
                );
                let mut discard_stream_events = false;
                if result.ok {
                    if let Some(snapshot) = inbox_snapshot.as_ref() {
                        let added = team.registry.inbox_entries_added_since(snapshot);
                        if !added.is_empty() {
                            let persist_result = delivery_protocol_session_id
                                .as_deref()
                                .ok_or_else(|| {
                                    "Agent Team 尚未绑定父会话，内部消息不能可靠入队".to_string()
                                })
                                .and_then(|session_id| {
                                    delivery_protocol_store
                                        .record_internal_deliveries(session_id, added.clone())
                                });
                            match persist_result {
                                Ok(protocol_state) => {
                                    let settled_ids = added
                                        .iter()
                                        .map(|(_, entry)| entry.message.id.clone())
                                        .filter(|delivery_id| {
                                            protocol_state.settled_ids.contains(delivery_id)
                                        })
                                        .collect::<std::collections::BTreeSet<_>>();
                                    let cancelled_ids = added
                                        .iter()
                                        .map(|(_, entry)| entry.message.id.clone())
                                        .filter(|delivery_id| {
                                            protocol_state.cancelled_ids.contains(delivery_id)
                                        })
                                        .collect::<std::collections::BTreeSet<_>>();
                                    let mut terminal_ids = settled_ids.clone();
                                    terminal_ids.extend(cancelled_ids.iter().cloned());
                                    team.registry.remove_delivery_ids(&terminal_ids);
                                    if !terminal_ids.is_empty() {
                                        // 终态重放不再产生新的“已送达”事件。
                                        discard_stream_events = true;
                                    }
                                    if !cancelled_ids.is_empty() {
                                        result = crate::lifecycle::error_tool_result(
                                            &normalized_call.name,
                                            "该团队消息投递已取消，未重新入队",
                                        );
                                    }
                                }
                                Err(error) => {
                                    team.registry.restore_inbox_snapshot(snapshot.clone());
                                    result = crate::lifecycle::error_tool_result(
                                        &normalized_call.name,
                                        &format!("内部消息持久化失败，本次投递已回滚：{error}"),
                                    );
                                    discard_stream_events = true;
                                }
                            }
                        }
                    }
                }
                let dismissed_delivery_ids = if result.ok {
                    dismiss_context.map(|(handle, mut delivery_ids)| {
                        if let Some(handle) = handle {
                            handle
                                .cancel_flag
                                .store(true, std::sync::atomic::Ordering::Release);
                            handle
                                .shutdown_flag
                                .store(true, std::sync::atomic::Ordering::Release);
                            let _ = handle
                                .command_tx
                                .send(tiangong_core::core::command::Command::Shutdown);
                        }
                        delivery_ids.sort();
                        delivery_ids.dedup();
                        delivery_ids
                    })
                } else {
                    None
                };
                (result, dismissed_delivery_ids, discard_stream_events)
            }
        };
        // drop stream_tx 让 channel 关闭，然后同步 drain 所有事件经 feedback 投递。
        drop(stream_tx);
        if discard_stream_events {
            for _ in stream_rx {}
        } else {
            flush_stream_events(stream_rx, feedback_tx.as_ref());
        }

        if sync_result.ok {
            if let Some(delivery_ids) = dismissed_delivery_ids {
                self.submit_recorded_delivery_cancellation(delivery_ids);
            }
        }

        // send_message / broadcast_message：投递消息后，await 目标子 Agent 执行完成，
        // 子 Agent 汇报作为 ToolResult 返回（主 Agent 当轮可见）。这与 recall_memory
        // 等 await 型工具一致——主 Agent 工具循环阻塞在此直到子 Agent 完成。
        // 其他工具（create_agent / dismiss / lock / unlock / notify）不阻塞，直接返回。
        let needs_await =
            matches!(call.name.as_str(), "send_message" | "broadcast_message") && sync_result.ok;
        if !needs_await {
            return Box::pin(async move { Some(sync_result) });
        }

        // 解析 send_message 的目标 agent_id（send_message 是单个，broadcast 是多个）。
        let team = Arc::clone(&self.team);
        let storage_root = self.storage_root_snapshot();
        let runtime_engine = match self.runtime_engine_snapshot() {
            Some(e) => e,
            None => {
                return Box::pin(async move { Some(sync_result) });
            }
        };
        let prompt_config = match self.prompt_config_snapshot() {
            Some(p) => p,
            None => return Box::pin(async move { Some(sync_result) }),
        };
        let Some(feedback) = feedback_tx else {
            return Box::pin(async move { Some(sync_result) });
        };

        // 收集需要 await 的目标 agent_id（Idle 且收件箱有待处理消息）。
        let target_ids: Vec<String> = match call.name.as_str() {
            "send_message" => collect_send_message_targets(&team, call),
            "broadcast_message" => collect_broadcast_targets(&team, &current_agent_id),
            _ => Vec::new(),
        };

        if target_ids.is_empty() {
            return Box::pin(async move { Some(sync_result) });
        }

        // 子 Agent 之间的消息异步调度：当前 Agent 仍占用一个并发槽，若在工具内
        // 等待另一个 Agent，满并发时会形成循环等待。Main Agent 才同步等待汇报。
        if current_agent_id != MAIN_AGENT_ID {
            self.schedule_pending_agents(target_ids);
            return Box::pin(async move { Some(sync_result) });
        }

        let tools = parent_tools.clone();
        let usage_tx = feedback.clone();
        let execution_semaphore = self.execution_semaphore();
        let token_budget = self.sub_agent_token_budget();
        let parent_session_id = prompt_config.session_id.clone();
        let stopping = self.stopping_flag();
        let delivery_commit_gate = self.delivery_commit_gate();
        let Some(scheduler_context) = self.scheduler_context() else {
            return Box::pin(async move { Some(sync_result) });
        };
        let continuation_targets = target_ids.clone();
        Box::pin(async move {
            let turn_result = if target_ids.len() == 1 {
                crate::team_bridge::run_agent_turn(
                    Arc::clone(&team),
                    target_ids.into_iter().next().unwrap(),
                    storage_root,
                    runtime_engine,
                    tools,
                    feedback,
                    prompt_config,
                    execution_semaphore,
                    token_budget,
                )
                .await
            } else {
                crate::team_bridge::run_agents_turns(
                    Arc::clone(&team),
                    target_ids,
                    storage_root,
                    runtime_engine,
                    tools,
                    feedback,
                    prompt_config,
                    execution_semaphore,
                    token_budget,
                )
                .await
            };
            // 上报子 Agent 的 token 用量到本轮主 Agent。
            // 子 Agent 的逐笔 TokenUsage 已由事件桥转发；这里只并入父 turn，
            // 避免桌面端再次收到总量事件后重复累计。
            usage_tx.accumulate_token_usage(turn_result.usage, "sub_agent_turn");
            let completed_delivery_ids = turn_result.completed_delivery_ids.clone();
            let acknowledged_main_message_ids = turn_result
                .main_messages
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let commit_result = if completed_delivery_ids.is_empty() {
                Ok(())
            } else {
                let commit_guard = delivery_commit_gate.clone().lock_owned().await;
                let injection = PluginFeedback::new(
                    "agent_team_sync_report",
                    json!({
                        "report": turn_result.report.clone(),
                        "messages": turn_result.main_messages.clone(),
                        "delivery_ids": completed_delivery_ids.clone(),
                        "cancelled": turn_result.cancelled,
                    }),
                );
                let mut retry_delay = std::time::Duration::from_millis(100);
                let committed = loop {
                    let commit = usage_tx.commit_pending_deliveries(
                        completed_delivery_ids.clone(),
                        vec![injection.clone()],
                    );
                    match tokio::time::timeout(std::time::Duration::from_secs(2), commit).await {
                        Ok(Ok(())) => break true,
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "提交同步 Agent 消息结果失败，稍后重试");
                        }
                        Err(_) => tracing::warn!("提交同步 Agent 消息结果超时，稍后重试"),
                    }
                    if stopping.load(std::sync::atomic::Ordering::Acquire) || usage_tx.is_closed() {
                        break false;
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(std::time::Duration::from_secs(2));
                };
                if !committed {
                    Err("会话已关闭，Sub Agent 结果保留待下次恢复结算".to_string())
                } else {
                    // Core ACK 返回后不再经过 await 边界，立即把 owned gate 移交给
                    // 已登记后台 finalizer；即使工具 Future 随后被丢弃，永久结算与
                    // outbox 清理仍会完成。
                    match scheduler_context.spawn_delivery_finalizer(
                        commit_guard,
                        parent_session_id.clone(),
                        completed_delivery_ids.clone(),
                        acknowledged_main_message_ids.clone(),
                    ) {
                        Ok(finalized) => match finalized.await {
                            Ok(true) => Ok(()),
                            Ok(false) => {
                                Err("会话已关闭，Sub Agent 结果保留待下次恢复结算".to_string())
                            }
                            Err(_) => {
                                Err("Agent 投递后台结算任务异常，结果保留待下次恢复".to_string())
                            }
                        },
                        Err(error) => Err(error),
                    }
                }
            };
            if commit_result.is_ok() && !completed_delivery_ids.is_empty() {
                scheduler_context.schedule(continuation_targets);
            }
            // 有稳定完成 ID 时，原子提交的 plugin injection 是报告唯一来源；原
            // ToolResult 只保留投递确认，避免模型在工具结果和注入中看到两份报告。
            // 没有完成 ID（取消/未进展）时仍直接返回状态说明。
            let mut result = sync_result;
            if completed_delivery_ids.is_empty() {
                if !turn_result.report.trim().is_empty() {
                    if !result.stdout.is_empty() {
                        result.stdout.push_str("\n\n---\n");
                    }
                    result.stdout.push_str(&turn_result.report);
                }
                if !turn_result.main_messages.is_empty() {
                    if !result.stdout.is_empty() {
                        result.stdout.push_str("\n\n---\n");
                    }
                    result
                        .stdout
                        .push_str("Sub Agent 发给 Main Agent 的消息：\n");
                    for message in turn_result.main_messages {
                        result.stdout.push_str("- ");
                        result.stdout.push_str(&message.content);
                        result.stdout.push('\n');
                    }
                }
            }
            if let Err(error) = commit_result {
                result.ok = false;
                result.exit_code = 1;
                result.summary = "Sub Agent 结果保存失败，已保留待恢复".to_string();
                if !result.stderr.is_empty() {
                    result.stderr.push('\n');
                }
                result.stderr.push_str(&error);
            }
            Some(result)
        })
    }
}

/// 从 send_message 调用解析目标 agent_id，仅返回当前 Idle（可立即派发）的。
fn collect_send_message_targets(team: &Arc<Mutex<TeamContext>>, call: &ToolCall) -> Vec<String> {
    let to = call
        .arguments
        .get("to")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .trim_start_matches('@')
        .to_string();
    if to.is_empty() || to == "main" {
        return Vec::new();
    }
    let Ok(team) = team.lock() else {
        return Vec::new();
    };
    let Some(agent) = team.registry.find_by_role(&to) else {
        return Vec::new();
    };
    // 仅 Idle 且非运行中的 Agent 需要 await（运行中的消息已实时注入，不重复启动）。
    if agent.status == AgentStatus::Idle
        && !team.is_agent_active(&agent.agent_id)
        && !team.is_in_flight(&agent.agent_id)
    {
        vec![agent.agent_id.clone()]
    } else {
        Vec::new()
    }
}

/// 从 broadcast_message 收集所有目标 agent_id（排除 exclude 列表 + 调用方自己），
/// 仅返回 Idle 且非运行中的。
fn collect_broadcast_targets(
    team: &Arc<Mutex<TeamContext>>,
    current_agent_id: &str,
) -> Vec<String> {
    let Ok(team) = team.lock() else {
        return Vec::new();
    };
    team.registry
        .alive_agents()
        .iter()
        .filter(|a| a.status == AgentStatus::Idle)
        .filter(|a| a.agent_id != current_agent_id)
        .filter(|a| !team.is_agent_active(&a.agent_id))
        .filter(|a| !team.is_in_flight(&a.agent_id))
        .map(|a| a.agent_id.clone())
        .collect()
}

/// 同步 drain `stream_rx` 中的所有 StreamEvent，经 `feedback_tx` 投递。
///
/// lifecycle handler 发出的事件先入本地 mpsc channel（保持 `&StdSender` 签名），
/// handler 返回后由本函数同步 drain 并经 feedback 投递。不再用独立转发线程，
/// 避免在 async 上下文中 `thread::spawn + join` 的时序问题。
/// `feedback_tx` 为 None（测试或极早期）时静默丢弃事件。
fn flush_stream_events(
    stream_rx: mpsc::Receiver<StreamEvent>,
    feedback_tx: Option<&PluginFeedbackTx>,
) {
    let Some(tx) = feedback_tx else {
        for _ in stream_rx {}
        return;
    };
    for event in stream_rx {
        tx.send_stream_event(event);
    }
}

// 静默未使用 import 警告（Arc/Mutex 在插件结构体字段使用，本文件仅引用 trait）。
#[allow(unused_imports)]
use {Arc as _, Mutex as _};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::message_bus::AgentInboxEntry;
    use crate::{AgentDescriptor, AgentMessage, AgentStatus, MessagePriority};
    use tiangong_core::core::Plugin;

    fn register_agent(plugin: &AgentTeamPlugin, agent_id: &str, role: &str, status: AgentStatus) {
        plugin
            .team
            .lock()
            .unwrap()
            .registry
            .register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: role.to_string(),
                label: role.to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status,
            });
    }

    fn register_active_actor(
        plugin: &AgentTeamPlugin,
        agent_id: &str,
        work_id: &str,
        cancelled: bool,
    ) {
        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        plugin.team.lock().unwrap().register_active_agent(
            agent_id.to_string(),
            command_tx,
            Arc::new(std::sync::atomic::AtomicBool::new(cancelled)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Some(work_id.to_string()),
        );
    }

    #[tokio::test]
    async fn dismiss_keeps_agent_and_inbox_when_cancel_ledger_cannot_persist() {
        let root = tempfile::tempdir().unwrap();
        let blocked_root = root.path().join("not-a-directory");
        std::fs::write(&blocked_root, b"blocked").unwrap();
        let plugin = AgentTeamPlugin::new(blocked_root);
        let mut parent = Session::new("parent");
        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);
        let agent_id = "agent-dev";
        {
            let mut team = plugin.team.lock().unwrap();
            team.registry.register(AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: "dev".to_string(),
                label: "Developer".to_string(),
                system_prompt: "work".to_string(),
                tools: Vec::new(),
                status: AgentStatus::Idle,
            });
            team.registry.deliver_inbox_entry(
                agent_id,
                AgentInboxEntry {
                    message: AgentMessage {
                        id: "delivery-1".to_string(),
                        from: "user".to_string(),
                        to: agent_id.to_string(),
                        content: "keep me".to_string(),
                        priority: MessagePriority::Normal,
                        created_at: "2026-07-12 12:00:00".to_string(),
                    },
                    additional_content: Vec::new(),
                    session_message_id: Some("message-1".to_string()),
                },
            );
        }
        let call = ToolCall {
            id: "dismiss-call".to_string(),
            name: "dismiss_agent".to_string(),
            arguments: json!({ "role": "dev" }),
        };

        let result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
            &plugin,
            &call,
            &mut parent,
            MAIN_AGENT_ID,
        )
        .await
        .unwrap();

        assert!(!result.ok);
        assert!(result.stderr.contains("Agent 未解散"));
        let team = plugin.team.lock().unwrap();
        assert!(team.registry.find_by_role("dev").is_some());
        assert_eq!(
            team.registry.pending_delivery_ids_for(agent_id),
            ["delivery-1"]
        );
    }

    #[tokio::test]
    async fn internal_send_is_durable_and_tool_retry_keeps_one_work_item() {
        let root = tempfile::tempdir().unwrap();
        let plugin = AgentTeamPlugin::new(root.path().to_path_buf());
        let mut parent = Session::new("parent");
        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);
        register_agent(&plugin, "agent-source", "source", AgentStatus::Running);
        register_agent(&plugin, "agent-target", "target", AgentStatus::Idle);
        register_active_actor(&plugin, "agent-source", "source-work", false);
        let call = ToolCall {
            id: "send-call-1".to_string(),
            name: "send_message".to_string(),
            arguments: json!({ "to": "target", "content": "do it" }),
        };

        for _ in 0..2 {
            let result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
                &plugin,
                &call,
                &mut parent,
                "agent-source",
            )
            .await
            .unwrap();
            assert!(result.ok, "{result:?}");
        }

        let team = plugin.team.lock().unwrap();
        let delivery_ids = team.registry.pending_delivery_ids_for("agent-target");
        assert_eq!(delivery_ids.len(), 1, "同一工具调用重试不得复制内部任务");
        let delivery_id = delivery_ids[0].clone();
        drop(team);
        let protocol = plugin
            .delivery_protocol_store()
            .load_state(&parent.id)
            .unwrap();
        let durable = protocol
            .pending_internal_deliveries
            .get(&delivery_id)
            .unwrap();
        assert_eq!(durable.target_agent_id, "agent-target");
        assert_eq!(durable.entry.message.content, "do it");
    }

    #[tokio::test]
    async fn internal_send_rolls_back_memory_when_protocol_persist_fails() {
        let root = tempfile::tempdir().unwrap();
        let blocked_root = root.path().join("not-a-directory");
        std::fs::write(&blocked_root, b"blocked").unwrap();
        let plugin = AgentTeamPlugin::new(blocked_root);
        let mut parent = Session::new("parent");
        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);
        register_agent(&plugin, "agent-source", "source", AgentStatus::Running);
        register_agent(&plugin, "agent-target", "target", AgentStatus::Idle);
        register_active_actor(&plugin, "agent-source", "source-work", false);
        let call = ToolCall {
            id: "send-call-failed".to_string(),
            name: "send_message".to_string(),
            arguments: json!({ "to": "target", "content": "must persist" }),
        };

        let result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
            &plugin,
            &call,
            &mut parent,
            "agent-source",
        )
        .await
        .unwrap();

        assert!(!result.ok);
        assert!(result.stderr.contains("本次投递已回滚"));
        assert!(plugin
            .team
            .lock()
            .unwrap()
            .registry
            .pending_delivery_ids_for("agent-target")
            .is_empty());
    }

    #[tokio::test]
    async fn cancelled_actor_cannot_apply_team_tool_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let plugin = AgentTeamPlugin::new(root.path().to_path_buf());
        let mut parent = Session::new("parent");
        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);
        register_agent(&plugin, "agent-source", "source", AgentStatus::Running);
        register_agent(&plugin, "agent-target", "target", AgentStatus::Idle);
        register_active_actor(&plugin, "agent-source", "source-work", true);
        let call = ToolCall {
            id: "send-after-cancel".to_string(),
            name: "send_message".to_string(),
            arguments: json!({ "to": "target", "content": "too late" }),
        };

        let result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
            &plugin,
            &call,
            &mut parent,
            "agent-source",
        )
        .await
        .unwrap();

        assert!(!result.ok);
        assert!(result.stderr.contains("已取消"));
        assert!(plugin
            .team
            .lock()
            .unwrap()
            .registry
            .pending_delivery_ids_for("agent-target")
            .is_empty());
        assert!(plugin
            .delivery_protocol_store()
            .load_state(&parent.id)
            .unwrap()
            .pending_internal_deliveries
            .is_empty());
    }

    #[tokio::test]
    async fn terminal_internal_delivery_replays_do_not_reenter_memory() {
        let root = tempfile::tempdir().unwrap();
        let plugin = AgentTeamPlugin::new(root.path().to_path_buf());
        let mut parent = Session::new("parent");
        <AgentTeamPlugin as Plugin>::on_session_ready(&plugin, &mut parent);
        register_agent(&plugin, "agent-source", "source", AgentStatus::Running);
        register_agent(&plugin, "agent-target", "target", AgentStatus::Idle);
        register_active_actor(&plugin, "agent-source", "source-work", false);
        let call_for = |id: &str| ToolCall {
            id: id.to_string(),
            name: "send_message".to_string(),
            arguments: json!({ "to": "target", "content": "retry" }),
        };
        let delivery_id_for = |call_id: &str| {
            format!(
                "team-tool:{}:{}:{}:{}:{}:{}",
                "agent-source".len(),
                "agent-source",
                call_id.len(),
                call_id,
                "agent-target".len(),
                "agent-target"
            )
        };
        let settled_call = call_for("settled-call");
        let settled_id = delivery_id_for(&settled_call.id);
        plugin
            .delivery_protocol_store()
            .settle(&parent.id, std::slice::from_ref(&settled_id))
            .unwrap();

        let settled_result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
            &plugin,
            &settled_call,
            &mut parent,
            "agent-source",
        )
        .await
        .unwrap();
        assert!(settled_result.ok);
        assert!(plugin
            .team
            .lock()
            .unwrap()
            .registry
            .pending_delivery_ids_for("agent-target")
            .is_empty());

        let cancelled_call = call_for("cancelled-call");
        let cancelled_id = delivery_id_for(&cancelled_call.id);
        plugin
            .delivery_protocol_store()
            .record_cancelled(&parent.id, [cancelled_id])
            .unwrap();
        let cancelled_result = <AgentTeamPlugin as ToolOverrideHandler>::handle(
            &plugin,
            &cancelled_call,
            &mut parent,
            "agent-source",
        )
        .await
        .unwrap();
        assert!(!cancelled_result.ok);
        assert!(cancelled_result.stderr.contains("已取消，未重新入队"));
        assert!(plugin
            .team
            .lock()
            .unwrap()
            .registry
            .pending_delivery_ids_for("agent-target")
            .is_empty());
    }
}
