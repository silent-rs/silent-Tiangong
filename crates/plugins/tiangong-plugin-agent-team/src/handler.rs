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
//! - 流事件经 `PluginFeedbackTx::send_stream_event` 投递（原 core 直接 `stream_tx.send`）。
//!   为复用原 lifecycle 的 `&StdSender<StreamEvent>` 签名（便于单测），handler 内部
//!   桥接一个 `mpsc::channel`，由转发线程把事件经 feedback 投递。
//! - `current_agent_id` 取自 engine 当前身份（经插件字段回填，见 [`crate::plugin`]）。

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::StreamEvent;

use crate::lifecycle::execute_team_tool;
use crate::plugin::AgentTeamPlugin;
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
        session: &Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // 桥接：lifecycle handler 发出的 StreamEvent 先入本地 mpsc channel（保持原
        // &StdSender 签名，便于单测），执行完后同步 drain 并经 feedback 投递。
        // 不再用独立转发线程（避免 thread::spawn + join 在 async 上下文中的时序问题）。
        let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
        let feedback_tx = self.feedback_tx();

        // 取当前调用方身份（经 session.active_agent_id 识别子 Agent）+ 父工具快照。
        let current_agent_id = self.current_agent_id(session);
        let parent_tools = self.parent_tools_snapshot();

        // 锁 team，执行工具（投递消息 / 注册 Agent / 锁文件等，同步部分）。
        let sync_result = {
            let Ok(mut team) = self.team.lock() else {
                let err = crate::lifecycle::error_tool_result(&call.name, "团队状态锁定失败");
                return Box::pin(async move { Some(err) });
            };
            execute_team_tool(
                &mut team,
                &current_agent_id,
                call,
                session,
                &parent_tools,
                &stream_tx,
            )
        };
        // drop stream_tx 让 channel 关闭，然后同步 drain 所有事件经 feedback 投递。
        drop(stream_tx);
        flush_stream_events(stream_rx, feedback_tx.as_ref());

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

        let tools = parent_tools.clone();
        let usage_tx = feedback.clone();
        Box::pin(async move {
            let turn_result = if target_ids.len() == 1 {
                crate::team_bridge::run_agent_turn(
                    team,
                    target_ids.into_iter().next().unwrap(),
                    runtime_engine,
                    tools,
                    feedback,
                    prompt_config,
                )
                .await
            } else {
                crate::team_bridge::run_agents_turns(
                    team,
                    target_ids,
                    runtime_engine,
                    tools,
                    feedback,
                    prompt_config,
                )
                .await
            };
            // 上报子 Agent 的 token 用量到本轮主 Agent。
            usage_tx.report_token_usage(turn_result.usage, "sub_agent_turn");
            // 把子 Agent 汇报追加到原 ToolResult 的 stdout。
            let mut result = sync_result;
            if !result.stdout.is_empty() {
                result.stdout.push_str("\n\n---\n");
            }
            result.stdout.push_str(&turn_result.report);
            Some(result)
        })
    }
}

impl AgentTeamPlugin {}

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
        && !team.active_agent_senders().contains_key(&agent.agent_id)
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
        .filter(|a| !team.active_agent_senders().contains_key(&a.agent_id))
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
