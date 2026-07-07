//! 子 Agent 调度（spawn-per-message）。
//!
//! 迁自 core `react/team_bridge.rs`，但调度模型简化为 **spawn-per-message**：
//! 当一个 Idle Agent 的收件箱收到消息时，由触发该投递的工具 handler 调用
//! [`spawn_agent_turn`] 异步启动该 Agent 的 `execute_turn`。不再有原 core 的
//! `drain_sub_agent_inboxes` 同步轮询循环、`dispatch_waker`、`active_agent_senders`。
//!
//! 子 Agent ↔ 主 Agent 的所有通信经 feedback 通道：
//! - **流事件**（Delta/ToolStart/ToolResult/...）：经 `PluginFeedbackTx::send_stream_event`
//!   转发到主 worker 的 stream 出口（UI 实时看到子 Agent 输出）。
//! - **token 用量**：经 `PluginFeedbackTx::report_token_usage` 即时累加到本轮主 Agent
//!   的 usage（并入 `Done.usage`）。
//! - **最终汇报**：经 `PluginFeedbackTx::inject_tool` 注入主 Agent 会话（以 tool result
//!   形式，主 Agent 下一轮即可看到子 Agent 的产出）。
//!
//! 子 Agent ↔ 子 Agent 的通信统一走 message bus（`TeamContext.registry` 收件箱）：
//! `send_message` / `broadcast_message` 把消息投到目标 Agent 收件箱；若目标 Agent 处于
//! Idle 且尚未被派发，handler 会触发 [`spawn_agent_turn`]。

use std::sync::{Arc, Mutex};

use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::model::TokenUsage;
use tiangong_core::react::engine::ReactEngine;
use tiangong_core::react::message::{inject_tool_to_session, INJECTION_TOOL_NAME};
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::session::{MessageRole, Session};
use tiangong_types::StreamEvent;

use crate::constants::{SUB_AGENT_MAX_OUTER_ITERATIONS, SUB_AGENT_MAX_TOOL_ROUNDS};
use crate::lifecycle::{persist_child_session, route_user_mentions_with_media};
use crate::state::{AgentMessage, AgentStatus};
use crate::TeamContext;

/// 派发指定 Agent：取出其收件箱中累积的消息，构造子 ReactEngine 并异步执行一个
/// 完整 turn，结果经 feedback 回报。
///
/// 调用时机：`send_message` / `broadcast_message` / `route_user_mentions` 投递消息到
/// 目标 Agent 后，若该 Agent 当前 Idle，则触发本函数。
///
/// 并发约束由调用方（插件）的信号量承担（见 [`crate::plugin::AgentTeamPlugin`]）。
/// 本函数立即返回（spawn 后不阻塞）；子 Agent 的输出、用量、汇报经 feedback 异步
/// 投递。
#[allow(clippy::too_many_arguments)]
pub fn spawn_agent_turn(
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
    runtime_engine: RuntimeEngine,
    parent_tools: Vec<tiangong_core::model::ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
) {
    // 取出收件箱消息 + child session；把 Agent 置 Running。
    let (combined, mut child_session, agent_label, agent_role) = {
        let Ok(mut team) = team.lock() else {
            return;
        };
        let messages = team.registry.drain_inbox(&agent_id);
        if messages.is_empty() {
            return;
        }
        let Some(child_session) = team.registry.get_session(&agent_id).cloned() else {
            return;
        };
        let descriptor = match team.registry.get(&agent_id) {
            Some(d) => d.clone(),
            None => return,
        };
        team.registry.update_status(&agent_id, AgentStatus::Running);
        let _ = feedback_tx.send_stream_event(StreamEvent::AgentStatusChanged {
            agent_id: agent_id.clone(),
            label: descriptor.label.clone(),
            status: "running".to_string(),
        });
        let combined = messages
            .into_iter()
            .map(|m| format!("[from:{} at {}]\n{}", m.from, m.created_at, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        (combined, child_session, descriptor.label, descriptor.role)
    };

    let team_for_task = Arc::clone(&team);
    let agent_id_for_task = agent_id.clone();
    let label_for_task = agent_label.clone();
    let role_for_task = agent_role.clone();
    let prompt_for_task = Arc::clone(&prompt_config);
    let feedback_for_usage = feedback_tx.clone();
    let feedback_for_status = feedback_tx.clone();
    let feedback_for_report = feedback_tx.clone();
    let feedback_for_events = feedback_tx.clone();
    let tools_for_task = filter_sub_agent_tools(parent_tools, &agent_id, &team);

    // 构造子 ReactEngine，共享父 RuntimeEngine（继承 tool_overrides）。
    let mut sub_engine = ReactEngine::new(
        runtime_engine,
        tools_for_task,
        SUB_AGENT_MAX_TOOL_ROUNDS,
        SUB_AGENT_MAX_OUTER_ITERATIONS,
    )
    .with_agent_id(agent_id.clone());

    // 子 Agent 首条消息：累积的 inbox 内容。
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = cmd_tx.send(Command::Message {
        content: combined,
        message_id: Some(scru128::new().to_string()),
        media: Vec::new(),
    });
    let keep_cmd_tx = cmd_tx;

    // 子 Agent 运行在独立的 stream channel 上；转发线程把事件翻译为主级事件
    // 经 feedback 投递。
    let (child_stream_tx, child_stream_rx) = std::sync::mpsc::channel();
    let forwarder = spawn_sub_agent_stream_forwarder(
        agent_id.clone(),
        role_for_task,
        label_for_task.clone(),
        feedback_for_events,
        child_stream_rx,
    );

    tokio::spawn(async move {
        let _keep_cmd_tx = keep_cmd_tx;
        let usage = sub_engine
            .execute_turn(&mut child_session, "", &child_stream_tx, &mut cmd_rx)
            .await;
        drop(child_stream_tx);
        let _ = forwarder.join();

        // 上报 token 用量到主 Agent 本轮（并入 Done.usage）。
        feedback_for_usage.report_token_usage(usage, format!("sub_agent:{agent_id_for_task}"));

        // 收集子 Agent 的总结输出作为汇报内容。
        let report = build_agent_report(&child_session, &label_for_task);

        // 经 feedback 注入主 Agent 会话（tool result 形式），主 Agent 下一轮可见。
        let payload = serde_json::json!({
            "agent_id": agent_id_for_task,
            "agent_label": label_for_task,
            "report": report,
        });
        feedback_for_report.inject_tool(SUB_AGENT_REPORT_TOOL, payload);

        // 回写 child session + 状态 Idle；持久化。
        let status = if report.contains("执行出错") {
            "error"
        } else {
            "idle"
        };
        {
            let Ok(mut team) = team_for_task.lock() else {
                return;
            };
            team.registry.set_session(&agent_id_for_task, child_session);
            team.registry
                .update_status(&agent_id_for_task, AgentStatus::Idle);
            if let Some(child) = team_for_task
                .lock()
                .ok()
                .and_then(|t| t.registry.get_session(&agent_id_for_task).cloned())
            {
                // persist 需要父 session.id；从 prompt_config 取（简化：用 agent 的
                // parent_session_id 字段不可达，这里用 prompt_config.session_id 兜底）。
                let mut dummy = Session::new(&label_for_task);
                dummy.id = prompt_for_task.session_id.clone();
                persist_child_session(&dummy, &agent_id_for_task, &child);
            }
        }
        let _ = feedback_for_status.send_stream_event(StreamEvent::AgentStatusChanged {
            agent_id: agent_id_for_task,
            label: label_for_task,
            status: status.to_string(),
        });
    });
}

/// 子 Agent 汇报注入使用的工具名（伪 tool_call，仅用于把子 Agent 产出注入主会话）。
pub const SUB_AGENT_REPORT_TOOL: &str = "sub_agent_report";

/// 子 Agent system prompt 构建所需的配置快照（由插件在 register 时捕获）。
///
/// `base` 经 `Arc` 共享（`SystemPromptConfig` 未实现 Clone，且体积较大）。
pub struct PromptConfig {
    /// 当前会话 id（持久化 child session 用）。
    pub session_id: String,
    /// 基础 system prompt 配置（由 `SystemPromptConfig::from_configs` 构建）。
    pub base: Arc<tiangong_core::prompt::SystemPromptConfig>,
}

/// 从父工具集中过滤出子 Agent 可用的工具（排除团队管理工具）。
fn filter_sub_agent_tools(
    parent_tools: Vec<tiangong_core::model::ToolSpec>,
    agent_id: &str,
    team: &Arc<Mutex<TeamContext>>,
) -> Vec<tiangong_core::model::ToolSpec> {
    let tool_names: Vec<String> = team
        .lock()
        .ok()
        .and_then(|t| t.registry.get(agent_id).map(|d| d.tools.clone()))
        .unwrap_or_default();
    parent_tools
        .into_iter()
        .filter(|t| tool_names.iter().any(|n| n == &t.name))
        .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
        .collect()
}

/// 构建子 Agent 汇报内容（总结阶段输出优先，回退到所有 Assistant 消息）。
fn build_agent_report(child_session: &Session, agent_label: &str) -> String {
    let summary = {
        let summaries: Vec<String> = child_session
            .messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::Assistant
                    && m.phase == tiangong_core::session::MessagePhase::Summary
            })
            .filter_map(|m| {
                let c = m.text_content().trim().to_string();
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        if summaries.is_empty() {
            child_session
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Assistant)
                .filter_map(|m| {
                    let c = m.text_content().trim().to_string();
                    if c.is_empty() {
                        None
                    } else {
                        Some(c)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            summaries.join("\n")
        }
    };

    if summary.is_empty() {
        format!("[{agent_label}] 已完成本轮工作，但没有生成文本输出。")
    } else {
        let brief = if summary.chars().count() > 500 {
            format!("{}...", summary.chars().take(500).collect::<String>())
        } else {
            summary
        };
        format!("[{agent_label}] 执行完成\n{brief}")
    }
}

/// 启动一个独立线程，把子 Agent 的内部 StreamEvent 流经 feedback 转发。
///
/// 子 Agent 的细粒度事件（Delta/ToolStart/ToolResult/...）经 `send_stream_event`
/// 投递；token 用量事件改经 `report_token_usage`（避免重复计入）。团队级事件
/// （AgentCreated/StatusChanged/...）透传。
fn spawn_sub_agent_stream_forwarder(
    agent_id: String,
    _agent_role: String,
    agent_label: String,
    feedback_tx: PluginFeedbackTx,
    child_rx: std::sync::mpsc::Receiver<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for event in child_rx {
            match event {
                StreamEvent::TokenUsage { usage, .. } => {
                    // 子 Agent 的 token 用量改走 report_token_usage（即时记账）。
                    feedback_tx.report_token_usage(usage, format!("sub_agent:{agent_id}"));
                }
                StreamEvent::Error { message } => {
                    let _ = feedback_tx.send_stream_event(StreamEvent::AgentNotification {
                        agent_id: agent_id.clone(),
                        agent_label: agent_label.clone(),
                        content: format!("执行出错：{message}"),
                        level: "error".to_string(),
                    });
                }
                StreamEvent::Done { .. } | StreamEvent::Retry { .. } => {
                    // 子 Agent 的 Done 不透传（避免主 Agent 误判 turn 结束）。
                }
                other => {
                    let _ = feedback_tx.send_stream_event(other);
                }
            }
        }
    })
}

/// 路由用户 @提及到目标 Agent，并为每个命中的 Idle Agent 触发派发。
///
/// 供 `Plugin::on_turn_started` 调用：解析用户输入开头的 @提及，把消息投到目标
/// Agent 收件箱并 spawn 其 turn。返回是否命中 @路由（命中则主 Agent 本轮应跳过
/// 常规回复——但插件层无法直接控制 engine，改由主 Agent 自行从注入的汇报判断）。
#[allow(clippy::too_many_arguments)]
pub fn route_mentions_and_spawn(
    team: Arc<Mutex<TeamContext>>,
    content: &str,
    runtime_engine: RuntimeEngine,
    parent_tools: Vec<tiangong_core::model::ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
) -> bool {
    let (tx, _rx) = std::sync::mpsc::channel();
    let targets = {
        let Ok(mut team) = team.lock() else {
            return false;
        };
        if !route_user_mentions_with_media(&mut team, content, Vec::new(), &tx) {
            return false;
        }
        // 收集被路由到的 Idle Agent。
        team.registry
            .alive_agents()
            .iter()
            .filter(|a| a.status == AgentStatus::Idle)
            .map(|a| a.agent_id.clone())
            .collect::<Vec<_>>()
    };

    for agent_id in targets {
        spawn_agent_turn(
            Arc::clone(&team),
            agent_id,
            runtime_engine.clone(),
            parent_tools.clone(),
            feedback_tx.clone(),
            Arc::clone(&prompt_config),
        );
    }
    true
}

// 静默未使用 import 警告（INJECTION_TOOL_NAME 保留供后续 report 注入格式对齐）。
#[allow(unused_imports)]
use {inject_tool_to_session as _, INJECTION_TOOL_NAME as _};
// 静默未使用 import 警告（AgentMessage 在 dispatch_agent_message 路径使用）。
#[allow(unused_imports)]
use AgentMessage as _;
// 静默未使用 import 警告（TokenUsage 在 report_token_usage 路径使用）。
#[allow(unused_imports)]
use TokenUsage as _;
