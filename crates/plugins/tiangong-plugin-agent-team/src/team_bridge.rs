//! 子 Agent 调度：在团队工具 handler 内 await 子 Agent 的 `execute_turn`。
//!
//! 调度模型与插件的其他工具一致——主 Agent 调用 `send_message` / `broadcast_message`
//! 时，handler 内部 await 目标子 Agent 的完整 ReAct turn，子 Agent 的汇报作为
//! ToolResult 返回（主 Agent 当轮即可看到产出）。这与 `recall_memory` 等 await 型
//! 工具完全同构，主 Agent 的工具循环天然阻塞等待。
//!
//! `create_agent` 不触发执行（注册后立即返回，不阻塞）；只有 `send_message` /
//! `broadcast_message` 投递消息到 Idle Agent 时才在 handler 内 await 其执行。
//!
//! 子 Agent ↔ 主 Agent 的通信：
//! - **流事件**：经 `PluginFeedbackTx::send_stream_event` 转发（UI 实时看到子 Agent 输出）。
//! - **token 用量**：子 Agent `execute_turn` 的返回值经 `report_token_usage` 上报。
//! - **汇报内容**：作为 ToolResult.stdout 直接返回给主 Agent（当轮可见）。
//!
//! 子 Agent ↔ 子 Agent 通信走 message bus（`TeamContext.registry` 收件箱 +
//! `active_agent_senders` 实时注入运行中的 Agent）。

use std::sync::{Arc, Mutex};

use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::model::{TokenUsage, ToolSpec};
use tiangong_core::react::engine::ReactEngine;
use tiangong_core::session::{MessageRole, Session};
use tiangong_types::StreamEvent;

use crate::constants::{SUB_AGENT_MAX_OUTER_ITERATIONS, SUB_AGENT_MAX_TOOL_ROUNDS};
use crate::lifecycle::persist_child_session;
use crate::state::AgentStatus;
use crate::TeamContext;

/// 子 Agent system prompt 构建所需的配置快照（由插件在 register 时捕获）。
///
/// `base` 经 `Arc` 共享（`SystemPromptConfig` 未实现 Clone，且体积较大）。
pub struct PromptConfig {
    /// 当前会话 id（持久化 child session 用）。
    pub session_id: String,
    /// 基础 system prompt 配置（由 `SystemPromptConfig::from_configs` 构建）。
    pub base: Arc<tiangong_core::prompt::SystemPromptConfig>,
}

/// 子 Agent turn 的执行结果。
pub struct AgentTurnResult {
    /// 子 Agent 汇报内容（作为 ToolResult.stdout 返回主 Agent）。
    pub report: String,
    /// 子 Agent 执行消耗的 token 总量（主 Agent 经 report_token_usage 上报）。
    pub usage: TokenUsage,
    /// 是否被取消。
    pub cancelled: bool,
}

/// 运行指定 Idle 子 Agent 的一个完整 turn，await 其完成后返回汇报。
///
/// 在 `send_message` / `broadcast_message` 的 handler 内 await 调用。主 Agent 的
/// 工具循环因此阻塞在此，直到子 Agent 完成（与 recall_memory 等 await 型工具一致）。
///
/// 本函数执行：
/// 1. 锁 team，取出收件箱消息 + child_session，置 Running，注册 cmd_tx 到
///    `active_agent_senders`（供 cancel 路由）。
/// 2. 构造子 ReactEngine（共享父 RuntimeEngine，继承 tool_overrides），发送首条
///    Command::Message（累积的 inbox 内容），`select!` 同时 await execute_turn 与
///    cancel 信号。
/// 3. 完成后回写 child_session、置 Idle、持久化、注销 cmd_tx。
///
/// 若 Agent 已是 Running（active_agent_senders 命中），消息经 dispatch_agent_message
/// 已实时注入其当前循环，本函数不重复启动，返回空汇报。
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_turn(
    team: Arc<Mutex<TeamContext>>,
    agent_id: String,
    runtime_engine: tiangong_core::runtime::RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    _prompt_config: Arc<PromptConfig>,
) -> AgentTurnResult {
    // 1. 锁 team，取收件箱 + child_session，置 Running，注册 cmd_tx。
    let (combined, mut child_session, agent_label) = {
        let Ok(mut team) = team.lock() else {
            return AgentTurnResult {
                report: "团队状态锁定失败".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
            };
        };
        // 若 Agent 已在运行，消息已实时注入，不重复启动。
        if team.active_agent_senders().contains_key(&agent_id) {
            return AgentTurnResult {
                report: format!(
                    "{agent_label_running} 正在执行，消息已实时送达",
                    agent_label_running = team
                        .registry
                        .get(&agent_id)
                        .map(|d| d.label.as_str())
                        .unwrap_or("Agent")
                ),
                usage: TokenUsage::default(),
                cancelled: false,
            };
        }
        let messages = team.registry.drain_inbox(&agent_id);
        if messages.is_empty() {
            return AgentTurnResult {
                report: "无待处理消息".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
            };
        }
        let Some(child_session) = team.registry.get_session(&agent_id).cloned() else {
            return AgentTurnResult {
                report: "子 Agent 会话缺失".to_string(),
                usage: TokenUsage::default(),
                cancelled: false,
            };
        };
        let label = match team.registry.get(&agent_id) {
            Some(d) => d.label.clone(),
            None => {
                return AgentTurnResult {
                    report: "子 Agent 已注销".to_string(),
                    usage: TokenUsage::default(),
                    cancelled: false,
                }
            }
        };
        team.registry.update_status(&agent_id, AgentStatus::Running);
        let _ = feedback_tx.send_stream_event(StreamEvent::AgentStatusChanged {
            agent_id: agent_id.clone(),
            label: label.clone(),
            status: "running".to_string(),
        });
        let combined = messages
            .into_iter()
            .map(|m| format!("[from:{} at {}]\n{}", m.from, m.created_at, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        (combined, child_session, label)
    };

    // 2. 构造子 ReactEngine + cmd 通道。
    // 子 Agent 用独立的 TurnUsageSink，避免其 execute_turn 的 bind() 覆盖主 Agent
    // 的 binding（主 Agent 阻塞在 handle().await 等 子 Agent，binding 不能被踢掉）。
    let sub_tools = filter_sub_agent_tools(parent_tools, &agent_id, &team);
    let sub_sink = std::sync::Arc::new(tiangong_core::core::plugin::TurnUsageSink::new());
    let sub_runtime = runtime_engine.with_turn_usage_sink(sub_sink);
    let mut sub_engine = ReactEngine::new(
        sub_runtime,
        sub_tools,
        SUB_AGENT_MAX_TOOL_ROUNDS,
        SUB_AGENT_MAX_OUTER_ITERATIONS,
    )
    .with_agent_id(agent_id.clone());

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = cmd_tx.send(Command::Message {
        content: combined,
        message_id: Some(scru128::new().to_string()),
        media: Vec::new(),
    });
    // 注册 cmd_tx 到 team（供 cancel 路由 + 运行中消息实时注入）。
    {
        if let Ok(mut team) = team.lock() {
            team.register_active_agent(agent_id.clone(), cmd_tx.clone());
        }
    }

    // 子 Agent stream 事件经独立 channel + 转发线程 → feedback。
    let (child_stream_tx, child_stream_rx) = std::sync::mpsc::channel::<StreamEvent>();
    let forwarder = spawn_sub_agent_stream_forwarder(
        agent_id.clone(),
        agent_label.clone(),
        feedback_tx.clone(),
        child_stream_rx,
    );

    // 3. await execute_turn（主 Agent 工具循环阻塞在此）。
    let keep_cmd_tx = cmd_tx;
    let usage = sub_engine
        .execute_turn(&mut child_session, "", &child_stream_tx, &mut cmd_rx)
        .await;
    drop(child_stream_tx);
    drop(keep_cmd_tx);
    let _ = forwarder.join();

    // 4. 回写 child_session + 置 Idle + 持久化 + 注销 cmd_tx。
    let report = build_agent_report(&child_session, &agent_label);
    let status = if report.contains("执行出错") {
        "error"
    } else {
        "idle"
    };
    {
        let Ok(mut team) = team.lock() else {
            return AgentTurnResult {
                report,
                usage,
                cancelled: false,
            };
        };
        team.registry.set_session(&agent_id, child_session);
        team.registry.update_status(&agent_id, AgentStatus::Idle);
        team.unregister_active_agent(&agent_id);
        // 持久化 child_session：child 自带 parent_session_id，用它定位 agents 目录。
        if let Some(child) = team.registry.get_session(&agent_id).cloned() {
            if let Some(parent_id) = child.parent_session_id.as_ref() {
                let mut parent = Session::new(&agent_label);
                parent.id = parent_id.clone();
                persist_child_session(&parent, &agent_id, &child);
            }
        }
    }
    let _ = feedback_tx.send_stream_event(StreamEvent::AgentStatusChanged {
        agent_id: agent_id.clone(),
        label: agent_label,
        status: status.to_string(),
    });

    AgentTurnResult {
        report,
        usage,
        cancelled: false,
    }
}

/// 并发运行多个子 Agent（broadcast 场景），await 全部完成后汇总汇报。
///
/// 每个子 Agent 独立 `run_agent_turn`，用 `FuturesUnordered` 并发驱动。汇总各 Agent
/// 的汇报合并为单条文本，usage 累加。
pub async fn run_agents_turns(
    team: Arc<Mutex<TeamContext>>,
    agent_ids: Vec<String>,
    runtime_engine: tiangong_core::runtime::RuntimeEngine,
    parent_tools: Vec<ToolSpec>,
    feedback_tx: PluginFeedbackTx,
    prompt_config: Arc<PromptConfig>,
) -> AgentTurnResult {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    if agent_ids.is_empty() {
        return AgentTurnResult {
            report: "无目标 Agent".to_string(),
            usage: TokenUsage::default(),
            cancelled: false,
        };
    }

    let mut futures: FuturesUnordered<_> = agent_ids
        .into_iter()
        .map(|agent_id| {
            let team = Arc::clone(&team);
            let tools = parent_tools.clone();
            let fb = feedback_tx.clone();
            let pc = Arc::clone(&prompt_config);
            Box::pin(run_agent_turn(
                team,
                agent_id,
                runtime_engine.clone(),
                tools,
                fb,
                pc,
            ))
                as std::pin::Pin<Box<dyn std::future::Future<Output = AgentTurnResult> + Send>>
        })
        .collect();

    let mut reports = Vec::new();
    let mut total_usage = TokenUsage::default();
    let mut any_cancelled = false;
    while let Some(result) = futures.next().await {
        reports.push(result.report);
        total_usage.accumulate(&result.usage);
        any_cancelled |= result.cancelled;
    }

    AgentTurnResult {
        report: reports.join("\n\n"),
        usage: total_usage,
        cancelled: any_cancelled,
    }
}

/// 从父工具集中过滤出子 Agent 可用的工具（排除团队管理工具）。
fn filter_sub_agent_tools(
    parent_tools: Vec<ToolSpec>,
    agent_id: &str,
    team: &Arc<Mutex<TeamContext>>,
) -> Vec<ToolSpec> {
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

/// 启动转发线程：把子 Agent 的 StreamEvent 经 feedback 投递。
///
/// - `TokenUsage` → `report_token_usage`（即时记账到本轮主 Agent）。
/// - `Done` / `Retry` → 抑制（子 Agent 的 Done 不透传，避免主 Agent 误判 turn 结束）。
/// - `Error` → 转为 `AgentNotification`。
/// - 其他 → 原样透传。
fn spawn_sub_agent_stream_forwarder(
    agent_id: String,
    agent_label: String,
    feedback_tx: PluginFeedbackTx,
    child_rx: std::sync::mpsc::Receiver<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for event in child_rx {
            match event {
                StreamEvent::TokenUsage { usage, .. } => {
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
                StreamEvent::Done { .. } | StreamEvent::Retry { .. } => {}
                other => {
                    let _ = feedback_tx.send_stream_event(other);
                }
            }
        }
    })
}
