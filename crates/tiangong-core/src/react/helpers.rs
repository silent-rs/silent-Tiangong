//! ReAct 主循环的通用辅助函数。
//!
//! 这些函数不依赖 `ReactEngine` 自身状态（无 `&self`），而是操作传入的
//! `Session` / `RuntimeEngine` / 命令通道，属于与主状态机解耦的纯过程性逻辑：
//! - 命令排空（`drain_pending_commands_async`）
//! - 非阻塞取消检查（`check_cancel`）
//! - 最终回答启发式判断（`looks_like_final_answer`）
//!
//! 浏览器页面自动观察已随 PageFetcher 能力下沉迁入 browser 插件（#225），
//! core 不再感知浏览器快照注入。

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::{Command, PendingCommandEffect};
use crate::react::message::{RuntimeMessageDisposition, accept_runtime_user_message};
use crate::runtime::RuntimeEngine;
use crate::session::Session;
use tiangong_types::StreamEvent;

/// 判断 ReAct 阶段的文本回复是否「看起来像一个完整回答」（而非向用户提问）。
///
/// 用于智能提升：当本轮执行过工具、但模型已给出实质文本，且不像是反问用户
/// （以问号结尾或显式请求输入）时，直接把它作为最终回复，避免总结阶段再生成
/// 一个更精简、反而丢失细节的版本。
///
/// 判据纯粹基于「是否在向用户提问」的语义，不依赖长度阈值——任务完成的简短
/// 确认（如「已创建定时提醒：每天 9 点叫你起床。」）同样是合法的最终回复。
pub(super) fn looks_like_final_answer(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // 以问号结尾 → 多为向用户提问，不作为最终回答。
    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return false;
    }
    // 显式请求用户提供信息：仅在文本以这些短语开头时才排除。
    // （较长文本里偶尔出现这些词通常是正常叙述，不应误判。）
    let intro = trimmed.chars().take(16).collect::<String>();
    let ask_intro = [
        "请问",
        "请提供",
        "请确认",
        "请选择",
        "你想",
        "你希望",
        "你是否",
    ];
    if ask_intro.iter().any(|p| intro.starts_with(p)) {
        return false;
    }
    true
}

/// 非阻塞排空命令队列，处理排队的用户命令（消息注入/取消/上下文压缩等）。
pub(super) fn drain_pending_commands_async(
    session: &mut Session,
    engine: &RuntimeEngine,
    agent_id: &str,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    turn_trust_mode: Option<&crate::core::TurnTrustModeController>,
) -> PendingCommandEffect {
    let commands = std::iter::from_fn(|| cmd_rx.try_recv().ok());
    process_commands(
        session,
        engine,
        agent_id,
        stream_tx,
        commands,
        turn_trust_mode,
    )
}

/// 处理工具执行期间暂存的命令；工具结果闭合后再调用以保持 Provider 消息顺序。
pub(super) fn process_buffered_commands(
    session: &mut Session,
    engine: &RuntimeEngine,
    agent_id: &str,
    stream_tx: &StdSender<StreamEvent>,
    commands: Vec<Command>,
    turn_trust_mode: Option<&crate::core::TurnTrustModeController>,
) -> PendingCommandEffect {
    process_commands(
        session,
        engine,
        agent_id,
        stream_tx,
        commands,
        turn_trust_mode,
    )
}

fn process_commands(
    session: &mut Session,
    engine: &RuntimeEngine,
    agent_id: &str,
    stream_tx: &StdSender<StreamEvent>,
    commands: impl IntoIterator<Item = Command>,
    turn_trust_mode: Option<&crate::core::TurnTrustModeController>,
) -> PendingCommandEffect {
    let mut current_agent_input = None;
    let mut plugin_routed = false;

    for cmd in commands {
        match cmd {
            Command::Cancel => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            command @ Command::PluginControl { .. } => {
                if agent_id == "main" {
                    engine.handle_plugin_runtime_command(&command);
                }
            }
            Command::Shutdown => return PendingCommandEffect::Shutdown,
            Command::Message {
                prepared,
                message_id,
                trust_mode_override,
                persistence_ack,
            } => {
                match accept_runtime_user_message(
                    engine,
                    agent_id,
                    session,
                    stream_tx,
                    message_id,
                    prepared,
                    persistence_ack,
                ) {
                    Ok(RuntimeMessageDisposition::CurrentAgentInput(text)) => {
                        if let Some(controller) = turn_trust_mode {
                            controller.apply_message_override(trust_mode_override);
                        }
                        current_agent_input = Some(text);
                    }
                    Ok(RuntimeMessageDisposition::RoutedToPlugin) => {
                        plugin_routed = true;
                    }
                    Err(err) => tracing::warn!(
                        error = %err,
                        "排空队列时追加用户消息持久化失败"
                    ),
                }
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                crate::core::apply_session_cwd(session);
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "工作目录已更新，本轮已安全中断，请重新发送消息".to_string(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::UpdateSessionMetadata {
                update,
                persistence_ack,
            } => {
                let trust_mode = engine.permission_gate().trust_mode_handle();
                if let Err(error) = crate::core::apply_session_metadata_update(
                    session,
                    &trust_mode,
                    update,
                    persistence_ack,
                ) {
                    tracing::warn!(%error, "执行中更新会话元数据失败");
                }
            }
            Command::ReloadConfig => {}
            command @ Command::Approval { .. } => {
                if agent_id == "main" {
                    engine.handle_plugin_runtime_command(&command);
                }
            }
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::defer_tool_injection(session, stream_tx, tool_name, payload);
            }
            Command::CommitPluginDeliveries {
                delivery_ids,
                tool_injections,
                persistence_ack,
            } => {
                if let Err(error) = crate::react::message::commit_plugin_deliveries(
                    session,
                    stream_tx,
                    delivery_ids,
                    tool_injections,
                    persistence_ack,
                ) {
                    tracing::warn!(%error, "提交插件持久投递失败");
                }
            }
            Command::CompressContext => {
                let _ = stream_tx.send(StreamEvent::AgentNotification {
                    agent_id: "system".to_string(),
                    agent_label: "系统".to_string(),
                    content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试".to_string(),
                    level: "warning".to_string(),
                });
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, stream_tx, engine);
            }
            Command::EmitStreamEvent(ev) => {
                let ev = *ev;
                let _ = stream_tx.send(ev);
            }
        }
    }

    if current_agent_input.is_some() || plugin_routed {
        PendingCommandEffect::MessagesInjected {
            current_agent_input,
            agent_routed: plugin_routed,
        }
    } else {
        PendingCommandEffect::None
    }
}

/// 检查是否收到取消信号。
///
/// 读取独立的 `cancel_flag`（由 deliver(Cancel) 在发送命令前设置），不排空命令
/// 队列——队列中的所有命令（Message / CompressContext / ResetContext 等）严格
/// 按提交顺序保留，由随后的 drain_pending_commands_async 处理。这彻底避免了
/// 回灌导致的乱序和压缩/清理提前执行问题。
pub(super) fn check_cancel(cancel_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> bool {
    use std::sync::atomic::Ordering;
    cancel_flag.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::core::Plugin;
    use crate::core_config::ModelEndpoint;
    use crate::model::SingleProviderClient;
    use crate::permission::{TrustMode, TrustModeHandle};
    use crate::react::test_support::{
        RecordedRuntimeCommand, approval, plugin_control, runtime_with_recorder,
    };
    use crate::session::PendingPluginDelivery;
    use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

    struct RoutingPlugin;

    impl ToolOverrideHandler for RoutingPlugin {}
    impl ToolSpecProvider for RoutingPlugin {}
    impl PromptSectionProvider for RoutingPlugin {}

    impl Plugin for RoutingPlugin {
        fn id(&self) -> &str {
            "test-routing"
        }

        fn plan_plugin_deliveries(
            &self,
            _actor_id: &str,
            source_message_id: &str,
            _prepared: &[tiangong_types::ContentBlock],
        ) -> Vec<PendingPluginDelivery> {
            vec![PendingPluginDelivery {
                delivery_id: format!("delivery-{source_message_id}"),
                source_message_id: source_message_id.to_string(),
                plugin_id: self.id().to_string(),
                target_id: "test-target".to_string(),
                content: "routed".to_string(),
                created_at: crate::session::now_text(),
                additional_content: Vec::new(),
            }]
        }

        fn dispatch_plugin_deliveries(&self, _session: &Session, _source_message_id: &str) -> bool {
            true
        }
    }

    fn runtime_with_plugin(plugin: Arc<dyn Plugin>) -> RuntimeEngine {
        RuntimeEngine::for_react_test(
            SingleProviderClient::new(ModelEndpoint {
                base_url: "http://127.0.0.1:1/v1".to_string(),
                api_key: "test-key".to_string(),
                model: "test-model".to_string(),
                timeout_ms: 1_000,
                ..Default::default()
            }),
            plugin,
        )
    }

    #[test]
    fn rejected_runtime_message_does_not_change_turn_trust_override() {
        let (engine, _) = runtime_with_recorder("http://127.0.0.1:1/v1".to_string());
        let mut session = Session::new("runtime-message-rejected-trust");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        let trust_mode = TrustModeHandle::new(TrustMode::Supervised);
        let turn = crate::core::TurnTrustModeGuard::new(trust_mode.clone(), None);
        let invalid = tiangong_types::ContentBlock::Media {
            kind: tiangong_types::MediaKind::Image,
            url: "data:image/png;base64,INVALID".to_string(),
            mime_type: Some("image/png".to_string()),
            title: None,
        };

        let effect = process_buffered_commands(
            &mut session,
            &engine,
            "main",
            &stream_tx,
            vec![Command::Message {
                prepared: vec![invalid],
                message_id: Some("invalid-message".to_string()),
                trust_mode_override: Some(TrustMode::FullTrust),
                persistence_ack: None,
            }],
            Some(&turn.controller()),
        );

        assert!(matches!(effect, PendingCommandEffect::None));
        assert_eq!(trust_mode.current(), TrustMode::Supervised);
    }

    #[test]
    fn plugin_routed_runtime_message_does_not_change_current_agent_trust() {
        let storage_root = std::env::temp_dir().join(format!(
            "tiangong-core-routed-message-trust-{}",
            scru128::new()
        ));
        std::fs::create_dir_all(&storage_root).unwrap();
        crate::storage::set_storage_root(storage_root);
        let engine = runtime_with_plugin(Arc::new(RoutingPlugin));
        let mut session = Session::new("runtime-message-routed-trust");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        let trust_mode = TrustModeHandle::new(TrustMode::Supervised);
        let turn = crate::core::TurnTrustModeGuard::new(trust_mode.clone(), None);

        let effect = process_buffered_commands(
            &mut session,
            &engine,
            "main",
            &stream_tx,
            vec![Command::Message {
                prepared: vec![tiangong_types::ContentBlock::text("@worker routed")],
                message_id: Some("routed-message".to_string()),
                trust_mode_override: Some(TrustMode::FullTrust),
                persistence_ack: None,
            }],
            Some(&turn.controller()),
        );

        assert!(matches!(
            effect,
            PendingCommandEffect::MessagesInjected {
                current_agent_input: None,
                agent_routed: true,
            }
        ));
        assert_eq!(trust_mode.current(), TrustMode::Supervised);
    }

    #[test]
    fn between_round_drain_forwards_plugin_runtime_commands() {
        let (engine, recorder) = runtime_with_recorder("http://127.0.0.1:1/v1".to_string());
        let mut session = Session::new("runtime-command-drain");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();

        let effect = process_buffered_commands(
            &mut session,
            &engine,
            "main",
            &stream_tx,
            vec![
                plugin_control("cancel-child"),
                approval("child-approval", true),
            ],
            None,
        );

        assert!(matches!(effect, PendingCommandEffect::None));
        assert_eq!(
            recorder.commands(),
            vec![
                RecordedRuntimeCommand::PluginControl {
                    plugin_id: "test-plugin".to_string(),
                    action: "cancel-child".to_string(),
                },
                RecordedRuntimeCommand::Approval {
                    request_id: "child-approval".to_string(),
                    approved: true,
                },
            ]
        );
    }

    #[test]
    fn nested_between_round_drain_does_not_rebroadcast_runtime_commands() {
        let (engine, recorder) = runtime_with_recorder("http://127.0.0.1:1/v1".to_string());
        let mut session = Session::new("nested-runtime-command-drain");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();

        let effect = process_buffered_commands(
            &mut session,
            &engine,
            "child-agent",
            &stream_tx,
            vec![
                plugin_control("cancel-sibling"),
                approval("sibling-approval", true),
            ],
            None,
        );

        assert!(matches!(effect, PendingCommandEffect::None));
        assert!(recorder.commands().is_empty());
    }

    #[test]
    fn check_cancel_preserves_queue_order() {
        // check_cancel 不应排空或重排命令队列——即使队列中有多个命令。
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // 放入消息1、消息2、压缩、取消
        tx.send(Command::Message {
            prepared: vec![tiangong_types::ContentBlock::text("msg1")],
            message_id: None,
            trust_mode_override: None,
            persistence_ack: None,
        })
        .unwrap();
        tx.send(Command::Message {
            prepared: vec![tiangong_types::ContentBlock::text("msg2")],
            message_id: None,
            trust_mode_override: None,
            persistence_ack: None,
        })
        .unwrap();
        tx.send(Command::CompressContext).unwrap();

        // check_cancel 返回 false（无取消信号）
        assert!(!check_cancel(&flag));

        // 队列应完整保留、顺序不变
        let c1 = rx.try_recv().unwrap();
        let c2 = rx.try_recv().unwrap();
        let c3 = rx.try_recv().unwrap();
        match c1 {
            Command::Message { prepared, .. } => {
                assert_eq!(tiangong_types::content_blocks_text(&prepared), "msg1")
            }
            _ => panic!("第一个应为 msg1"),
        }
        match c2 {
            Command::Message { prepared, .. } => {
                assert_eq!(tiangong_types::content_blocks_text(&prepared), "msg2")
            }
            _ => panic!("第二个应为 msg2"),
        }
        assert!(
            matches!(c3, Command::CompressContext),
            "第三个应为 CompressContext"
        );
    }

    #[test]
    fn check_cancel_true_when_signal_set() {
        // 即使队列中有消息，cancel_flag=true 时应立即返回 true，不排空队列
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Command::Message {
            prepared: vec![tiangong_types::ContentBlock::text("msg")],
            message_id: None,
            trust_mode_override: None,
            persistence_ack: None,
        })
        .unwrap();

        assert!(check_cancel(&flag));

        // 队列中的消息应仍然存在（check_cancel 不消费队列）
        assert!(
            rx.try_recv().is_ok(),
            "队列中的消息不应被 check_cancel 消费"
        );
    }

    #[test]
    fn check_cancel_reads_signal_not_queue() {
        // cancel_flag = true → 返回 true
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(check_cancel(&flag));
    }

    #[test]
    fn check_cancel_false_when_no_signal() {
        // cancel_flag = false → 返回 false（不排空队列）
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(!check_cancel(&flag));
    }

    #[test]
    fn looks_like_final_answer_empty_is_not_final() {
        // 空文本不视为完整回答。
        assert!(!looks_like_final_answer(""));
        assert!(!looks_like_final_answer("   "));
    }

    #[test]
    fn looks_like_final_answer_short_substantive_is_final() {
        // 短的非提问文本（任务完成的简短确认）同样视为最终回复。
        // 这是本次修复的核心：不再因长度不足把短回复送进总结阶段。
        assert!(looks_like_final_answer("好的，已完成。"));
        assert!(looks_like_final_answer(
            "已创建定时提醒：每个工作日 11:00 提醒你点外卖。"
        ));
    }

    #[test]
    fn looks_like_final_answer_long_substantive_is_final() {
        // 一段较长、不以问号结尾、不以提问短语开头的实质文本 → 视为完整回答。
        let text = "我重新检查了当前分支的全部改动，结论是核心问题已经修复。\
                    首先，AgentTurn 不再把整轮 elapsed_ms 当作深度思考耗时传给 ThinkingBlock，\
                    语义已经修正。其次，历史思考块固定 isActive 为 false，避免误计时与误展开。\
                    第三，showProcess 通过 useEffect 跟随 isActive 同步，完成后自动折叠过程。\
                    最后，summaryFrag 改为数组，多条非 react 助手回复不再互相覆盖。\
                    前端构建通过，整体改动合理，建议合并。";
        assert!(looks_like_final_answer(text));
    }

    #[test]
    fn looks_like_final_answer_ending_with_question_is_not_final() {
        // 以问号结尾 → 视为向用户提问，不作为最终回答（无论长短）。
        assert!(!looks_like_final_answer("请问需要我继续吗？"));
        assert!(!looks_like_final_answer(
            "我重新检查了代码，发现了一些问题，但还需要你确认以下几点？"
        ));
    }

    #[test]
    fn looks_like_final_answer_intro_question_phrase_is_not_final() {
        // 以提问短语开头 → 视为向用户提问，不作为最终回答（无论长短）。
        assert!(!looks_like_final_answer("请提供你的 API 凭据以便继续。"));
        assert!(!looks_like_final_answer("请确认以上配置是否正确。"));
        assert!(!looks_like_final_answer("你想使用哪种方案？"));
    }
}
