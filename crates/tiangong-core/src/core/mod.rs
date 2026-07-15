//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, Sender as StdSender};
use std::sync::{Arc, Mutex};
use std::thread::{self};
use tokio::sync::mpsc as tokio_mpsc;

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::{SingleProviderClient, ToolSpec};
use crate::react::message::accept_user_message;
use crate::session::{MessageRole, Session};
use tiangong_types::{ContentBlock, SessionStreamEvent, StreamEvent};

/// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
///
/// 30 轮：review 类多步骤任务（连续读取多个文件做审查）在 15 轮时容易触顶，
/// 过早进入总结阶段。30 轮给复杂任务更多空间；触顶后仍走与正常完成相同的总结
/// 路径（由模型自行判断完成度），无需特殊触顶逻辑。
const MAX_TOOL_ROUNDS: usize = 30;
/// 总结阶段后重新进入工具执行阶段的最大次数。
const MAX_OUTER_ITERATIONS: u32 = 3;

pub mod command;
pub(crate) use command::Command;
pub mod plugin;
pub use plugin::Plugin;
pub mod builder;
pub mod error;
pub mod storage_location;

pub use builder::TiangongCoreBuilder;
pub use error::CoreError;
pub use storage_location::CoreStorageLocation;

/// 天工智能体核心
pub struct TiangongCore {
    /// 用户命令发送端（tokio unbounded，send 不需要 await）
    cmd_tx: Option<tokio_mpsc::UnboundedSender<Command>>,
    /// worker task（共享 runtime 上的长驻 task；idle 时被 park 不占线程）
    worker_task: Option<tokio::task::JoinHandle<Session>>,
    /// 会话 ID
    session_id: String,
    /// 当前 Core 独立的配置提供者。
    ///
    /// 宿主可以按会话原子替换配置，不会影响同进程中的其他 Core。
    config: CoreConfigProvider,
    /// 会话信任模式基线。
    trust_mode: std::sync::Mutex<crate::permission::TrustMode>,
    /// 保证"设置取消信号 + 入队取消命令"与并发消息入队具有单一顺序。
    command_delivery_lock: Mutex<()>,
}

impl TiangongCore {
    /// Builder 的实际装配实现（私有）。
    ///
    /// worker task 由共享 runtime 的 `spawn` 创建（非 OS 线程），构造期不会失败；
    /// `build()` 的 `Result` 仅承载必填字段缺失的检查。空闲时 worker task 停在
    /// `cmd_rx.recv().await`，future 被 park、线程归还 runtime 池。
    fn assemble(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
        storage: CoreStorageLocation,
    ) -> Self {
        // Invariant: 无效 CWD 的会话应在 Core 创建前由调用方过滤。
        // 此处仅做防御性检查：若 session.cwd 非空且不是有效目录，记录告警。
        if !session.cwd.is_empty() && !std::path::Path::new(&session.cwd).is_dir() {
            tracing::warn!(
                cwd = %session.cwd,
                "invalid cwd: 会话应在 Core 创建前被过滤，插件可能行为异常"
            );
        }
        let storage_root = storage.into_root();
        let initial_trust_mode = session.trust_mode;
        let trust_mode = std::sync::Mutex::new(initial_trust_mode);
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();

        let worker_trust_mode = initial_trust_mode;
        let worker_config = config.clone();
        // clone 一份 cmd_tx 给 worker，用于在 register_plugin 时注入给插件
        // （作为状态反馈通道，复用同一命令通道，避免新增 channel）。
        let worker_cmd_tx = cmd_tx.clone();
        let worker_task = crate::shared_runtime::shared_runtime().spawn(worker_loop_async(
            worker_config,
            session,
            stream_tx,
            cmd_rx,
            worker_trust_mode,
            plugins,
            worker_cmd_tx,
            storage_root,
        ));

        Self {
            cmd_tx: Some(cmd_tx),
            worker_task: Some(worker_task),
            session_id,
            config,
            trust_mode,
            command_delivery_lock: Mutex::new(()),
        }
    }

    /// Builder 入口：与宿主入口（GUI/CLI/Server）解耦的构造方式。
    ///
    /// session 为必填字段，新会话由调用方创建后传入。
    pub fn builder() -> TiangongCoreBuilder {
        TiangongCoreBuilder::default()
    }

    fn send_cmd(&self, cmd: Command) -> Result<(), CoreError> {
        let Some(ref tx) = self.cmd_tx else {
            return Err(CoreError::WorkerStopped);
        };
        if tx.send(cmd).is_ok() {
            Ok(())
        } else {
            Err(CoreError::WorkerStopped)
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 替换该 Core 的独立配置。
    ///
    /// 配置在下一次 turn 构建 engine 时自动生效（每 turn 现建 engine 会读取最新
    /// generation），无需显式通知 worker。
    pub fn replace_config(&self, config: CoreConfig) -> Result<(), CoreError> {
        self.config.replace(config);
        Ok(())
    }

    /// 是否已停止（worker task 已结束或命令通道已关闭）。
    ///
    /// 已停止的 core 无法再接收命令，调用方应移除并按需重建。
    pub fn is_stopped(&self) -> bool {
        if self.cmd_tx.is_none() {
            return true;
        }
        self.worker_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(true)
    }

    /// 是否可接收命令（worker 存活且通道未关闭）。
    ///
    /// 注意：当前实现无法准确反映"是否正在执行 Agent Turn"（执行状态在 worker
    /// 线程内部，无共享标志），此处仅表达"可投递命令"，即 `!is_stopped()`。
    /// 真正的 busy 语义需后续 worker 状态上报完善。
    pub fn is_busy(&self) -> bool {
        !self.is_stopped()
    }

    /// 设置会话信任模式。
    ///
    /// 更新后对当前会话的权限门与插件实时生效。
    pub fn set_trust_mode(&self, mode: crate::permission::TrustMode) {
        *self.trust_mode.lock().unwrap() = mode;
    }

    /// 关闭并获取最终 session。
    ///
    /// worker panic 时返回 [`CoreError::WorkerPanicked`]，不再静默兜底为
    /// `Session::new("recovered")`——避免丢失原会话数据后调用方误判成功。
    pub fn into_session(self) -> Result<Session, CoreError> {
        self.shutdown_and_take_session()
    }

    /// 关闭 Core 并等待 worker 与插件结束钩子全部完成，不取回最终 session。
    pub fn shutdown_join(self) -> Result<(), CoreError> {
        self.shutdown_and_take_session().map(drop)
    }

    fn shutdown_and_take_session(mut self) -> Result<Session, CoreError> {
        let _ = self.send_cmd(Command::Shutdown);
        self.cmd_tx = None;
        if let Some(task) = self.worker_task.take() {
            // worker task 跑在共享 runtime 上；用 Handle::block_on 等待它结束。
            // 调用方通常已在 runtime 之外（spawn_blocking 包裹），可安全 block_on。
            let handle = crate::shared_runtime::shared_runtime().handle().clone();
            match handle.block_on(task) {
                Ok(session) => return Ok(session),
                Err(join_error) => {
                    if join_error.is_panic() {
                        tracing::warn!("TiangongCore worker task panic");
                    } else {
                        tracing::warn!(%join_error, "TiangongCore worker task 被取消");
                    }
                }
            }
        }
        Err(CoreError::WorkerPanicked)
    }
}

impl crate::agent_input::AgentInput for TiangongCore {
    fn deliver(&self, input: crate::agent_input::AgentInputKind) -> Result<(), CoreError> {
        use crate::agent_input::{AgentInputKind, ApprovalInput, CommandInput, MessageInput};

        let _delivery_guard = self
            .command_delivery_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        // 取消完全由 Command::Cancel 队列承载：select! 的 cmd_rx 分支和 drain
        // 会消费它并终止 turn。
        if matches!(&input, AgentInputKind::Command(CommandInput::Cancel)) {
            return self.send_cmd(Command::Cancel);
        }

        match input {
            AgentInputKind::Message(MessageInput::UserMessage {
                prepared,
                message_id,
            }) => self.send_cmd(Command::Message {
                prepared,
                message_id,
            }),
            AgentInputKind::Tool(tool) => self.send_cmd(Command::InjectTool {
                tool_name: tool.tool_name().to_string(),
                payload: tool.render(),
            }),
            AgentInputKind::Approval(ApprovalInput::Response {
                request_id,
                approved,
            }) => self.send_cmd(Command::Approval {
                request_id,
                approved,
            }),
            AgentInputKind::Command(cmd) => match cmd {
                CommandInput::Cancel => self.send_cmd(Command::Cancel),
                CommandInput::CompressContext => self.send_cmd(Command::CompressContext),
                CommandInput::ResetContext => self.send_cmd(Command::ResetContext),
            },
        }
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(Command::Shutdown);
        }
        self.cmd_tx = None;
    }
}

// ==================== 工作线程 ====================

/// 真正的 async 工作循环
#[allow(clippy::too_many_arguments)]
async fn worker_loop_async(
    config: CoreConfigProvider,
    mut session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    trust_mode: crate::permission::TrustMode,
    plugins: Vec<Arc<dyn Plugin>>,
    cmd_tx: tokio_mpsc::UnboundedSender<Command>,
    storage_root: std::path::PathBuf,
) -> Session {
    let session_id = session.id.clone();
    // session 的 storage_root 由调用方在构造时绑定（bind_storage_root）。
    // 若未绑定则用 Core 的 storage_root 作为回退。
    if session.bound_storage_root().is_none() {
        session.bind_storage_root(storage_root.clone());
    }
    // on_session_ready 仅在首次 engine build + 插件注册完成后触发一次（跨 turn）
    let mut session_ready_fired = false;

    // 会话级共享的插件注册表（跨 turn 复用，避免每 turn 重复注册）。
    // 每 turn 的 TurnContext 通过 Arc clone 共享同一份注册表。
    let shared_tool_overrides: Arc<
        Mutex<HashMap<String, Arc<dyn crate::tool_override::ToolOverrideHandler>>>,
    > = Arc::new(Mutex::new(HashMap::new()));
    let shared_prompt_section_providers: Arc<
        Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>,
    > = Arc::new(Mutex::new(Vec::new()));

    // IndexManager 已下沉到 index 插件私有持有，core 不再创建/感知它。
    // 索引的初始扫描、增量写入、finalize 全部由 index 插件的生命周期钩子接管。

    // 内部 StreamEvent 通道 —— 转发线程负责包装 session_id
    // stream_tx 保持 std::sync::mpsc（工具执行等同步代码可直接使用）
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let fwd_session_id = session_id.clone();
    let fwd_tx = external_tx.clone();
    // ReactEngine 内部仍可用 Done/Error 表达执行结果，但外部终态必须由 worker 在
    // 完成清理、插件钩子与持久化后统一发出。转发线程先捕获内部终态，通过屏障
    // 确认已消费完本轮事件；worker 最后只放行一个规范化终态。
    //
    // 屏障使用 tokio::sync::Notify：转发线程（std::thread）调 notify_waiters，
    // worker（async task）调 notified().await，无需 block_in_place。
    // 共享状态用 std::sync::Mutex（双方都短时持锁，不跨 await）。
    let turn_capture = Arc::new(TurnCaptureState {
        capture: Mutex::new(TurnCapture::default()),
        notify: tokio::sync::Notify::new(),
    });
    let fwd_capture = turn_capture.clone();
    let forward_terminal = Arc::new(AtomicBool::new(false));
    let fwd_terminal = forward_terminal.clone();
    let forward_handle = thread::spawn(move || {
        let mut external_open = true;
        while let Ok(event) = stream_rx.recv() {
            if let StreamEvent::TokenUsage {
                usage,
                agent_id,
                source,
                ..
            } = &event
            {
                #[allow(clippy::collapsible_if)]
                if let Ok(mut capture) = fwd_capture.capture.lock() {
                    capture.usage.record(usage, agent_id.as_deref(), source);
                }
            }
            match event {
                StreamEvent::TurnBoundary { boundary_id } => {
                    if let Ok(mut capture) = fwd_capture.capture.lock() {
                        capture.processed_boundary = capture.processed_boundary.max(boundary_id);
                    }
                    fwd_capture.notify.notify_waiters();
                    continue;
                }
                terminal @ (StreamEvent::Done { .. } | StreamEvent::Error { .. }) => {
                    if !fwd_terminal.swap(false, Ordering::AcqRel) {
                        if let Ok(mut capture) = fwd_capture.capture.lock() {
                            // 可恢复错误之后可能继续成功；以本轮最后一个内部终态为准。
                            capture.terminal = Some(terminal);
                        }
                        continue;
                    }
                    if external_open
                        && fwd_tx
                            .send(SessionStreamEvent {
                                session_id: fwd_session_id.clone(),
                                event: terminal,
                            })
                            .is_err()
                    {
                        // 即使宿主已断开，也必须继续消费内部事件并响应屏障，避免 worker
                        // 在会话关闭时永久等待。
                        external_open = false;
                    }
                    continue;
                }
                _ => {}
            }
            if external_open
                && fwd_tx
                    .send(SessionStreamEvent {
                        session_id: fwd_session_id.clone(),
                        event,
                    })
                    .is_err()
            {
                external_open = false;
            }
        }
    });
    let mut turn_boundary_id = 0u64;
    // 空闲/关闭阶段的后台插件事件不经过主 turn 提交；保留一份连续去重状态，
    // 既能即时持久化增量，也能识别随后到达的 cancelled 累计事件。
    let mut background_usage_capture = TurnUsageCapture::default();

    apply_session_cwd(&session);

    // 初始索引扫描已由 index 插件的 on_session_ready 钩子接管（见下方钩子调用）。

    // on_session_ready 已移至「首次 engine build + 插件注册完成」之后触发，
    // 确保插件在钩子内能读到已注入的 workspace / trust_mode / feedback。

    while let Some(cmd) = cmd_rx.recv().await {
        // 每 turn 现建 engine：engine/client 不跨 turn，配置变更靠下次 turn 新建
        // client 天然生效。engine 在 turn 结束时 drop，释放 LLM 连接资源。
        match cmd {
            Command::Message {
                prepared,
                message_id,
            } => {
                // 每 turn 现建 TurnContext + 注册插件 + 触发 on_session_ready（仅首次）。
                // client 在 turn 结束时 drop，不跨 turn 复用。
                let (mut ctx, _tools) = build_turn_context(
                    &config,
                    &stream_tx,
                    trust_mode,
                    &storage_root,
                    &plugins,
                    cmd_tx.clone(),
                    &mut session,
                    &mut session_ready_fired,
                    &shared_tool_overrides,
                    &shared_prompt_section_providers,
                );

                // 记录本轮起点；执行结束后用于计算 elapsed_ms 并写入用户消息（持久化，
                // 使历史会话同样展示执行总时长与状态）。
                let turn_started = std::time::Instant::now();
                let turn_start_cwd = session.cwd.clone();
                // 新轮次内部终态一律先捕获，直到最终会话状态提交完毕。
                forward_terminal.store(false, Ordering::Release);
                if let Ok(mut capture) = turn_capture.capture.lock() {
                    capture.terminal = None;
                    capture.usage = TurnUsageCapture::default();
                }
                background_usage_capture = TurnUsageCapture::default();
                // 宿主入口已完成输入准备。Core 原样持久化已就绪消息并确认，
                // 再进入 Agent Loop。
                let message_id = message_id.unwrap_or_else(|| scru128::new().to_string());
                let accepted = match accept_user_message(
                    &mut session,
                    &stream_tx,
                    Some(message_id),
                    prepared,
                    true,
                ) {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        send_final_stream_event(
                            &stream_tx,
                            &forward_terminal,
                            &turn_capture,
                            &mut turn_boundary_id,
                            StreamEvent::Error {
                                message: format!("用户消息持久化失败：{err}"),
                            },
                        )
                        .await;
                        continue;
                    }
                };
                let turn_start_idx = accepted.turn_start_idx;
                let user_msg_id = accepted.message_id.clone();

                // Turn 开始前：回调插件生命周期钩子（用户消息已写入 session）
                for plugin in &plugins {
                    plugin.on_turn_started(&mut session, turn_start_idx);
                }

                execute_turn_async(
                    &mut session,
                    &accepted.message_id,
                    &accepted.prepared,
                    &ctx,
                    &stream_tx,
                    &mut cmd_rx,
                    &session_id,
                )
                .await;

                // 等转发线程消费完 execute_turn_async 在返回前产生的所有事件，取得
                // 本轮最后一个内部终态。屏障不对外可见。
                turn_boundary_id = turn_boundary_id.wrapping_add(1);
                let boundary_id = turn_boundary_id;
                let _ = stream_tx.send(StreamEvent::TurnBoundary { boundary_id });
                // 等转发线程消费完 execute_turn_async 在返回前产生的所有事件，取得
                // 本轮最后一个内部终态。屏障不对外可见。
                let mut terminal = loop {
                    {
                        let mut capture = turn_capture
                            .capture
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        if capture.processed_boundary >= boundary_id {
                            break capture.terminal.take();
                        }
                    }
                    turn_capture.notify.notified().await;
                }
                .unwrap_or(StreamEvent::Done { usage: None });

                if session.cwd != turn_start_cwd {
                    let workspace_path = std::path::PathBuf::from(&session.cwd);
                    let workspace = workspace_path.is_dir().then_some(workspace_path.as_path());
                    for plugin in &plugins {
                        plugin.set_workspace(workspace);
                    }
                }

                // 无论哪个执行分支结束，都闭合尚未配对的工具调用，避免下一轮给
                // Provider 发送不合法的 Assistant(tool_calls) 历史。
                let interrupted_tools =
                    crate::react::message::close_unfinished_tool_calls_for_turn(&mut session);
                if !interrupted_tools.is_empty() {
                    for (tool_call_id, tool_name, output) in interrupted_tools {
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: tool_name,
                            tool_call_id: Some(tool_call_id),
                            ok: false,
                            output,
                            full_output: None,
                            duration_ms: None,
                        });
                    }
                    if matches!(terminal, StreamEvent::Done { .. }) {
                        terminal = StreamEvent::Error {
                            message: "本轮仍有未完成的工具调用，已安全中断".to_string(),
                        };
                    }
                }

                // 未完成工具已统一闭合，这里已经重新到达安全注入边界。审批等待期间
                // 提交的插件结果不能因随后取消或关闭而滞留到下一轮。
                crate::react::message::flush_deferred_tool_injections(&mut session, &stream_tx);

                // Turn 完成后：插件先提交自己的最终状态，再由 Core 保存整份会话。
                for plugin in &plugins {
                    plugin.on_turn_finished(&mut session, turn_start_idx);
                }

                // 插件终态钩子也可能同步上报用量。再次设置屏障，确保转发线程已捕获
                // 本轮全部 TokenUsage，再由 Core 统一写入权威 Session。
                wait_for_stream_boundary(&stream_tx, &turn_capture, &mut turn_boundary_id).await;
                let turn_usage = {
                    let mut capture = turn_capture
                        .capture
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    std::mem::take(&mut capture.usage)
                };
                // 本轮结束后后台子执行仍可能继续发送累计取消事件；继承本轮已经观测
                // 的 actor 基线，避免跨“主 turn → 空闲”边界重复记账。
                background_usage_capture = turn_usage.clone();
                turn_usage.apply_to_session(&mut session);

                // base64 等运行内容只服务本轮；历史图片由 Provider 按稳定本地引用重编码。
                session.clear_transient_content();

                // 轮次结束：将执行时长与终态写入用户消息（turn 锚点）并落盘。
                // 所有终态分支（成功/失败/取消）都会回到这里，故统一处理。
                let elapsed_ms = turn_started.elapsed().as_millis() as u64;
                let mut status = TurnOutcome::from_terminal(&terminal).status;

                // turn 被取消时通知插件响应取消意图（如中断子 Agent、暂停页面观察）。
                // 在 on_turn_finished 之前调用，让插件先响应取消再做统一收尾。
                if status == tiangong_types::TurnStatus::Cancelled {
                    for plugin in &plugins {
                        plugin.on_cancel(&mut session).await;
                    }
                }

                let mut user_msg_updated = false;
                if let Some(msg) = session
                    .messages
                    .iter_mut()
                    .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
                {
                    msg.set_turn_result(elapsed_ms, status);
                    user_msg_updated = true;
                }
                let persist_result = session.try_persist_to_disk();
                if let Err(err) = persist_result {
                    terminal = StreamEvent::Error {
                        message: format!("最终会话持久化失败：{err}"),
                    };
                    status = tiangong_types::TurnStatus::Failed;
                    if let Some(msg) = session
                        .messages
                        .iter_mut()
                        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
                    {
                        msg.set_turn_result(elapsed_ms, status);
                    }
                    // 尽力再保存一次失败状态；即使存储介质持续失败，也仍需向调用方
                    // 返回明确失败，不能伪装成功。
                    let _ = session.try_persist_to_disk();
                }
                if user_msg_updated {
                    crate::react::message::emit_session_message_upsert(
                        &session,
                        &stream_tx,
                        &user_msg_id,
                    );
                }

                send_final_stream_event(
                    &stream_tx,
                    &forward_terminal,
                    &turn_capture,
                    &mut turn_boundary_id,
                    terminal,
                )
                .await;

                // shutdown 由 Command::Shutdown 在下一轮 cmd_rx.recv() 时处理。
                // 置位（mod.rs:300），turn 会通过 cancel 信号尽快终止；终止后回到 recv
                // 取到 Command::Shutdown 即 break。

                // 反刍（Micro/Meta）已下沉到 memory 插件 on_turn_finished 钩子。

                // session 从 TurnContext 取回,后续 idle 命令(InjectTool 等)继续使用。
                session = session;
            }
            Command::Cancel => {
                // 活跃执行会在 select!/drain 中消费 Cancel 并终止 turn；
                // 空闲时到达此处说明 turn 已结束，无需处理。
            }
            Command::Approval { .. } => {
                // 空闲时收到审批响应说明用户点了过期的审批,忽略。
            }
            Command::Shutdown => {
                break;
            }
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::defer_tool_injection(
                    &mut session,
                    &stream_tx,
                    tool_name,
                    payload,
                );
                crate::react::message::flush_deferred_tool_injections(&mut session, &stream_tx);
                session.persist_to_disk();
                send_final_stream_event(
                    &stream_tx,
                    &forward_terminal,
                    &turn_capture,
                    &mut turn_boundary_id,
                    StreamEvent::Done { usage: None },
                )
                .await;
            }
            Command::EmitStreamEvent(ev) => {
                let ev = *ev;
                // 没有主 Agent turn 时，插件后台任务的用量事件会直接到达 worker。
                // 此时不存在后续 turn 终态可代为提交，必须在转发前由 Core 自己落盘。
                if let StreamEvent::TokenUsage {
                    usage,
                    agent_id,
                    source,
                    ..
                } = &ev
                {
                    let delta = background_usage_capture.record(usage, agent_id.as_deref(), source);
                    apply_usage_delta_to_session(&mut session, &delta, agent_id.as_deref());
                    session.updated_at = crate::session::now_text();
                    if let Err(error) = session.try_persist_to_disk() {
                        tracing::warn!(%error, "持久化插件后台用量失败");
                    }
                }
                if matches!(ev, StreamEvent::Done { .. } | StreamEvent::Error { .. }) {
                    send_final_stream_event(
                        &stream_tx,
                        &forward_terminal,
                        &turn_capture,
                        &mut turn_boundary_id,
                        ev,
                    )
                    .await;
                    background_usage_capture = TurnUsageCapture::default();
                } else {
                    let _ = stream_tx.send(ev);
                }
            }
            Command::CompressContext => {
                let (mut ctx, _tools) = build_turn_context(
                    &config,
                    &stream_tx,
                    trust_mode,
                    &storage_root,
                    &plugins,
                    cmd_tx.clone(),
                    &mut session,
                    &mut session_ready_fired,
                    &shared_tool_overrides,
                    &shared_prompt_section_providers,
                );
                compress_context_for_session(&mut session, &ctx, &stream_tx, &mut cmd_rx).await;
                continue;
            }
            Command::ResetContext => {
                let (mut ctx, _tools) = build_turn_context(
                    &config,
                    &stream_tx,
                    trust_mode,
                    &storage_root,
                    &plugins,
                    cmd_tx.clone(),
                    &mut session,
                    &mut session_ready_fired,
                    &shared_tool_overrides,
                    &shared_prompt_section_providers,
                );
                reset_context_for_session(&mut session, &stream_tx, &ctx);
                continue;
            }
        }
    }

    // worker 已退出主循环，关闭通道前做一次有界排空，确保已提交状态进入最终 Session。
    while let Ok(command) = cmd_rx.try_recv() {
        process_shutdown_feedback_command(
            &mut session,
            &stream_tx,
            command,
            &mut background_usage_capture,
            trust_mode,
        );
    }
    session.persist_to_disk();

    // engine 已在最后一个 turn 分支结束时 drop，其重试回调持有的 stream_tx clone
    // 随之释放。关闭内部通道，等待转发线程结束。
    drop(stream_tx);
    // 在 blocking pool 上 join 转发线程，避免阻塞 runtime worker 线程。
    let _ = tokio::task::spawn_blocking(move || forward_handle.join()).await;

    // 会话结束：回调插件生命周期钩子（index 插件在此 finalize Session 索引）。
    // 注意：stream 通道已关闭，钩子内不应再投递流事件。
    for plugin in &plugins {
        plugin.on_session_ended(&mut session);
    }

    session
}

fn process_shutdown_feedback_command(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    command: Command,
    usage_capture: &mut TurnUsageCapture,
    trust_mode: crate::permission::TrustMode,
) {
    let _ = trust_mode;
    match command {
        Command::InjectTool { tool_name, payload } => {
            crate::react::message::defer_tool_injection(session, stream_tx, tool_name, payload);
            crate::react::message::flush_deferred_tool_injections(session, stream_tx);
        }
        Command::EmitStreamEvent(event)
            if !matches!(*event, StreamEvent::Done { .. } | StreamEvent::Error { .. }) =>
        {
            let event = *event;
            if let StreamEvent::TokenUsage {
                usage,
                agent_id,
                source,
                ..
            } = &event
            {
                let delta = usage_capture.record(usage, agent_id.as_deref(), source);
                apply_usage_delta_to_session(session, &delta, agent_id.as_deref());
                session.updated_at = crate::session::now_text();
            }
            let _ = stream_tx.send(event);
        }
        Command::EmitStreamEvent(_) => {}
        _ => {}
    }
}

pub(crate) fn apply_session_cwd(session: &Session) {
    let cwd = session.cwd.trim();
    if cwd.is_empty() {
        tiangong_toolkit::set_session_cwd(None);
        return;
    }

    let path = std::path::PathBuf::from(cwd);
    if path.is_dir() {
        tiangong_toolkit::set_session_cwd(Some(path));
    }
}

pub(crate) async fn compress_context_for_session(
    session: &mut Session,
    ctx: &crate::turn_context::TurnContext,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) {
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages: session.messages.len(),
    });
    let organizer = crate::context::organizer::ContextOrganizer::new(ctx.context_limit)
        .with_keep_recent_turns(6);
    let update = tokio::select! {
        update = organizer.force_update_summary_with_usage_async(session, ctx.client()) => update,
        cmd = cmd_rx.recv() => {
            let _ = cmd;
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: "上下文压缩已取消".to_string(),
                level: "warning".to_string(),
            });
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Cancelled,
                summary_up_to: session.summary_up_to,
                remaining_messages: session.messages.len().saturating_sub(session.summary_up_to),
            });
            return;
        }
    };
    match update {
        Ok(update) => {
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            session.current_tokens = 0;
            session.active_agent_current_tokens = 0;
            session.agent_current_tokens.clear();
            crate::react::context::rebuild_system_prompt(session, ctx);
            crate::react::context::emit_token_usage(
                stream_tx,
                &update.usage,
                Some(0),
                ctx.context_limit,
                "manual_context_compress",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: if update.compressed {
                    tiangong_types::stream::ContextCompressAction::Compress
                } else {
                    tiangong_types::stream::ContextCompressAction::Noop
                },
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
        }
        Err(err) => {
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: format!("上下文压缩失败：{err}"),
                level: "error".to_string(),
            });
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Failed,
                summary_up_to: session.summary_up_to,
                remaining_messages: session.messages.len().saturating_sub(session.summary_up_to),
            });
        }
    }
}

pub(crate) fn reset_context_for_session(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    ctx: &crate::turn_context::TurnContext,
) {
    let total = session.messages.len();
    session.summary_up_to = total;
    crate::context::compressor::mark_compact_boundary(&mut session.messages, total);
    session.context_summary = None;
    session.current_tokens = 0;
    session.active_agent_current_tokens = 0;
    session.agent_current_tokens.clear();
    // 清空后重建 system prompt
    crate::react::context::rebuild_system_prompt(session, ctx);
    let _ = stream_tx.send(StreamEvent::ContextCompressed {
        action: tiangong_types::stream::ContextCompressAction::Clear,
        summary_up_to: total,
        remaining_messages: 0,
    });
    session.persist_to_disk();
}

/// 转发线程与 worker 之间的轮次终态/屏障状态。
#[derive(Default)]
struct TurnCapture {
    terminal: Option<StreamEvent>,
    usage: TurnUsageCapture,
    processed_boundary: u64,
}

/// 转发线程与 worker 共享的屏障状态。
///
/// `capture` 用 std::sync::Mutex 保护（双方都短时持锁，不跨 await）；
/// `notify` 让 worker（async task）在不使用 block_in_place 的情况下等待
/// 转发线程处理完 TurnBoundary。
struct TurnCaptureState {
    capture: Mutex<TurnCapture>,
    notify: tokio::sync::Notify,
}

/// 转发线程捕获的单轮精确用量。
///
/// `total` 同时包含主 Agent 与所有子 Agent；`by_agent` 额外保留子 Agent 维度，
/// 供会话切换到 Agent Tab 时恢复各自累计值。
#[derive(Default, Clone)]
struct TurnUsageCapture {
    total: tiangong_types::TokenUsage,
    by_agent: HashMap<String, tiangong_types::TokenUsage>,
    /// 每个执行单元已经观测到的用量，用于把显式标记为
    /// `source=cancelled-cumulative` 的累计快照换算成尚未落账的增量。
    /// `None` 表示主执行单元。普通 `cancelled-incremental` 事件始终按增量记账。
    observed_by_actor: HashMap<Option<String>, tiangong_types::TokenUsage>,
}

impl TurnUsageCapture {
    fn record(
        &mut self,
        usage: &tiangong_types::TokenUsage,
        agent_id: Option<&str>,
        source: &str,
    ) -> tiangong_types::TokenUsage {
        let mut normalized = usage.clone();
        if normalized.total_tokens == 0 {
            normalized.total_tokens = normalized.prompt_tokens + normalized.completion_tokens;
        }
        if normalized.total_tokens == 0 {
            return tiangong_types::TokenUsage::default();
        }

        let actor = agent_id.map(str::to_string);
        let observed = self.observed_by_actor.entry(actor).or_default();
        let delta = if source == "cancelled-cumulative" {
            let delta = cumulative_usage_delta(&normalized, observed);
            merge_cumulative_usage(observed, &normalized);
            delta
        } else {
            observed.accumulate(&normalized);
            normalized
        };

        self.total.accumulate(&delta);
        if let Some(agent_id) = agent_id {
            self.by_agent
                .entry(agent_id.to_string())
                .or_default()
                .accumulate(&delta);
        }
        delta
    }

    fn apply_to_session(self, session: &mut Session) {
        if self.total.total_tokens > 0 {
            session.token_usage.accumulate(&self.total);
        }
        for (agent_id, usage) in self.by_agent {
            session
                .agent_token_usage
                .entry(agent_id)
                .or_default()
                .accumulate(&usage);
        }
    }
}

fn cumulative_usage_delta(
    cumulative: &tiangong_types::TokenUsage,
    observed: &tiangong_types::TokenUsage,
) -> tiangong_types::TokenUsage {
    tiangong_types::TokenUsage {
        prompt_tokens: cumulative
            .prompt_tokens
            .saturating_sub(observed.prompt_tokens),
        completion_tokens: cumulative
            .completion_tokens
            .saturating_sub(observed.completion_tokens),
        total_tokens: cumulative
            .total_tokens
            .saturating_sub(observed.total_tokens),
        prompt_cache_hit_tokens: optional_usage_delta(
            cumulative.prompt_cache_hit_tokens,
            observed.prompt_cache_hit_tokens,
        ),
        prompt_cache_miss_tokens: optional_usage_delta(
            cumulative.prompt_cache_miss_tokens,
            observed.prompt_cache_miss_tokens,
        ),
    }
}

fn optional_usage_delta(cumulative: Option<usize>, observed: Option<usize>) -> Option<usize> {
    cumulative.map(|value| value.saturating_sub(observed.unwrap_or_default()))
}

fn merge_cumulative_usage(
    observed: &mut tiangong_types::TokenUsage,
    cumulative: &tiangong_types::TokenUsage,
) {
    observed.prompt_tokens = observed.prompt_tokens.max(cumulative.prompt_tokens);
    observed.completion_tokens = observed.completion_tokens.max(cumulative.completion_tokens);
    observed.total_tokens = observed
        .total_tokens
        .max(cumulative.total_tokens)
        .max(observed.prompt_tokens + observed.completion_tokens);
    observed.prompt_cache_hit_tokens = max_optional_usage(
        observed.prompt_cache_hit_tokens,
        cumulative.prompt_cache_hit_tokens,
    );
    observed.prompt_cache_miss_tokens = max_optional_usage(
        observed.prompt_cache_miss_tokens,
        cumulative.prompt_cache_miss_tokens,
    );
}

fn max_optional_usage(current: Option<usize>, cumulative: Option<usize>) -> Option<usize> {
    match (current, cumulative) {
        (Some(current), Some(cumulative)) => Some(current.max(cumulative)),
        (current, cumulative) => current.or(cumulative),
    }
}

fn apply_usage_delta_to_session(
    session: &mut Session,
    usage: &tiangong_types::TokenUsage,
    agent_id: Option<&str>,
) {
    if usage.total_tokens == 0 {
        return;
    }
    session.token_usage.accumulate(usage);
    if let Some(agent_id) = agent_id {
        session
            .agent_token_usage
            .entry(agent_id.to_string())
            .or_default()
            .accumulate(usage);
    }
}

#[cfg(test)]
mod turn_usage_capture_tests {
    use super::*;

    fn usage(
        prompt_tokens: usize,
        completion_tokens: usize,
        total_tokens: usize,
    ) -> tiangong_types::TokenUsage {
        tiangong_types::TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn normalizes_and_persists_main_and_agent_usage_once() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(10, 5, 0), None, "react-round-1");
        capture.record(&usage(20, 8, 28), Some("researcher"), "react-round-1");
        capture.record(&usage(7, 3, 10), Some("researcher"), "react-round-2");

        let mut session = Session::new("usage-capture");
        session.token_usage = usage(1, 1, 2);
        session
            .agent_token_usage
            .insert("researcher".to_string(), usage(2, 1, 3));
        capture.apply_to_session(&mut session);

        assert_eq!(session.token_usage.prompt_tokens, 38);
        assert_eq!(session.token_usage.completion_tokens, 17);
        assert_eq!(session.token_usage.total_tokens, 55);

        let agent_usage = session.agent_token_usage.get("researcher").unwrap();
        assert_eq!(agent_usage.prompt_tokens, 29);
        assert_eq!(agent_usage.completion_tokens, 12);
        assert_eq!(agent_usage.total_tokens, 41);
    }

    #[test]
    fn cancelled_cumulative_usage_only_records_unseen_delta() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(10, 5, 15), None, "react-round-1");
        let delta = capture.record(&usage(14, 8, 22), None, "cancelled-cumulative");

        assert_eq!(delta.prompt_tokens, 4);
        assert_eq!(delta.completion_tokens, 3);
        assert_eq!(delta.total_tokens, 7);

        let mut session = Session::new("cancelled-usage-capture");
        capture.apply_to_session(&mut session);
        assert_eq!(session.token_usage.prompt_tokens, 14);
        assert_eq!(session.token_usage.completion_tokens, 8);
        assert_eq!(session.token_usage.total_tokens, 22);
    }

    #[test]
    fn shutdown_background_usage_delta_can_be_applied_immediately() {
        let mut capture = TurnUsageCapture::default();
        let first = capture.record(&usage(20, 10, 30), Some("child-agent"), "react-round-1");
        let cancelled = capture.record(
            &usage(25, 12, 37),
            Some("child-agent"),
            "cancelled-cumulative",
        );
        let mut session = Session::new("shutdown-usage-capture");

        apply_usage_delta_to_session(&mut session, &first, Some("child-agent"));
        apply_usage_delta_to_session(&mut session, &cancelled, Some("child-agent"));

        assert_eq!(session.token_usage.total_tokens, 37);
        assert_eq!(
            session
                .agent_token_usage
                .get("child-agent")
                .unwrap()
                .total_tokens,
            37
        );
    }

    #[test]
    fn summary_cancelled_increment_is_added_after_react_usage() {
        let mut capture = TurnUsageCapture::default();
        capture.record(&usage(70, 30, 100), None, "react-round-1");
        capture.record(&usage(15, 5, 20), None, "cancelled-incremental");

        let mut session = Session::new("summary-cancelled-increment");
        capture.apply_to_session(&mut session);

        assert_eq!(session.token_usage.prompt_tokens, 85);
        assert_eq!(session.token_usage.completion_tokens, 35);
        assert_eq!(session.token_usage.total_tokens, 120);
    }

    #[test]
    fn shutdown_feedback_persists_incremental_and_cancelled_usage_once() {
        let mut session = Session::new("shutdown-feedback-usage");
        let mut capture = TurnUsageCapture::default();
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let trust_mode = session.trust_mode;

        for (usage, source) in [
            (usage(20, 10, 30), "react-round-1"),
            (usage(25, 12, 37), "cancelled-cumulative"),
        ] {
            process_shutdown_feedback_command(
                &mut session,
                &stream_tx,
                Command::EmitStreamEvent(Box::new(StreamEvent::TokenUsage {
                    usage,
                    current_tokens: None,
                    compression_threshold_tokens: None,
                    context_limit_tokens: None,
                    source: source.to_string(),
                    agent_id: Some("child-agent".to_string()),
                })),
                &mut capture,
                trust_mode,
            );
        }

        assert_eq!(session.token_usage.total_tokens, 37);
        assert_eq!(
            session
                .agent_token_usage
                .get("child-agent")
                .unwrap()
                .total_tokens,
            37
        );
        assert_eq!(stream_rx.try_iter().count(), 2);
    }
}

#[cfg(test)]
mod shared_runtime_tests {
    use super::*;
    use crate::core::storage_location::CoreStorageLocation;
    use crate::core_config::{CoreConfig, CoreConfigProvider};

    /// 多个 Core 共享同一个 runtime，空闲时各自停在 cmd_rx.recv().await。
    /// 验证构造期不 panic、Core 可正常 deliver + shutdown。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_cores_share_runtime_and_shutdown_cleanly() {
        let root = tempfile::tempdir().unwrap();

        let config = CoreConfigProvider::new(CoreConfig::default());
        let cores: Vec<TiangongCore> = (0..3)
            .map(|i| {
                let session = Session::new(&format!("shared-runtime-{i}"));
                let (event_tx, _event_rx) = std::sync::mpsc::channel();
                TiangongCore::builder()
                    .config(config.clone())
                    .session(session)
                    .event_sender(event_tx)
                    .storage(CoreStorageLocation::new(root.path()))
                    .build()
                    .unwrap()
            })
            .collect();

        // 所有 Core 初始即存活（worker task 已 spawn 但 idle 在 recv().await）
        for core in &cores {
            assert!(!core.is_stopped(), "Core 应存活");
        }

        // 逐个关闭，验证 shutdown 路径在共享 runtime 上正常工作
        for core in cores {
            let result = tokio::task::spawn_blocking(move || core.shutdown_join()).await;
            assert!(result.is_ok(), "shutdown_join 不应 panic");
        }
    }
}

async fn send_final_stream_event(
    stream_tx: &StdSender<StreamEvent>,
    forward_terminal: &AtomicBool,
    turn_capture: &Arc<TurnCaptureState>,
    turn_boundary_id: &mut u64,
    event: StreamEvent,
) {
    debug_assert!(matches!(
        event,
        StreamEvent::Done { .. } | StreamEvent::Error { .. }
    ));
    forward_terminal.store(true, Ordering::Release);
    if stream_tx.send(event).is_ok() {
        // 必须确认转发线程已经处理完这个终态，才能接收/重置下一轮。否则新轮次
        // 清除放行标志时，上一轮尚在队列里的终态可能被误当作内部事件吞掉。
        wait_for_stream_boundary(stream_tx, turn_capture, turn_boundary_id).await;
    }
    forward_terminal.store(false, Ordering::Release);
}

async fn wait_for_stream_boundary(
    stream_tx: &StdSender<StreamEvent>,
    turn_capture: &Arc<TurnCaptureState>,
    turn_boundary_id: &mut u64,
) {
    *turn_boundary_id = turn_boundary_id.wrapping_add(1);
    let boundary_id = *turn_boundary_id;
    if stream_tx
        .send(StreamEvent::TurnBoundary { boundary_id })
        .is_err()
    {
        return;
    }
    loop {
        {
            let capture = turn_capture
                .capture
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if capture.processed_boundary >= boundary_id {
                return;
            }
        }
        turn_capture.notify.notified().await;
    }
}

/// 转发线程捕获的单个对话轮次终态。
///
/// `status` 由终态事件推导：`Done` → Success；`Error` 文案含「取消/cancel/abort」
/// 时为 Cancelled，否则为 Failed。worker_loop_async 据此把 `status` 与执行时长
/// 写入用户消息，供前端（含历史会话）展示。
struct TurnOutcome {
    status: tiangong_types::TurnStatus,
}

impl TurnOutcome {
    fn from_terminal(event: &StreamEvent) -> Self {
        let status = match event {
            StreamEvent::Done { .. } => tiangong_types::TurnStatus::Success,
            StreamEvent::Error { message } => {
                let lower = message.to_lowercase();
                if lower.contains("取消")
                    || lower.contains("cancel")
                    || lower.contains("abort")
                    || lower.contains("中断")
                {
                    tiangong_types::TurnStatus::Cancelled
                } else {
                    tiangong_types::TurnStatus::Failed
                }
            }
            _ => tiangong_types::TurnStatus::Failed,
        };
        Self { status }
    }
}

/// 执行一个完整的对话轮次（可能多轮工具调用），async 版
async fn execute_turn_async(
    session: &mut Session,
    message_id: &str,
    prepared: &[ContentBlock],
    ctx: &crate::turn_context::TurnContext,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    session_id: &str,
) {
    let mut turn_ctx = ctx.clone();
    turn_ctx.agent_id = session_id.to_string();
    turn_ctx
        .execute_turn(session, Some((message_id, prepared)), stream_tx, cmd_rx)
        .await;
}

/// 每 turn 现建 TurnContext：从最新 config 快照构建 client/权限/用量，
/// 共享会话级插件注册表，注入 turn 级 feedback，收集 tool_specs。
///
/// - 插件注册表（tool_overrides / prompt_section_providers）是会话级共享的
///   Arc<Mutex>，只在首次 turn 注册，后续 turn 通过 Arc clone 复用。
/// - 每 turn 必须刷新的是：feedback_tx（绑定新的 TurnUsageSink）、exec_env、
///   permission_overrides、tool_specs（可能因配置变更而不同）。
#[allow(clippy::too_many_arguments)]
fn build_turn_context(
    config: &CoreConfigProvider,
    stream_tx: &StdSender<StreamEvent>,
    trust_mode: crate::permission::TrustMode,
    storage_root: &std::path::Path,
    plugins: &[Arc<dyn Plugin>],
    cmd_tx: tokio_mpsc::UnboundedSender<Command>,
    session: &mut Session,
    session_ready_fired: &mut bool,
    shared_tool_overrides: &Arc<
        Mutex<HashMap<String, Arc<dyn crate::tool_override::ToolOverrideHandler>>>,
    >,
    shared_prompt_section_providers: &Arc<
        Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>,
    >,
) -> (crate::turn_context::TurnContext, Vec<ToolSpec>) {
    let cfg = config.snapshot();
    let mut ctx =
        build_context_from_config(&cfg, stream_tx, trust_mode, storage_root.to_path_buf())
            // 注入会话级共享的插件注册表（Arc clone，跨 turn 复用同一份）。
            .with_shared_plugin_state(
                Arc::clone(shared_tool_overrides),
                Arc::clone(shared_prompt_section_providers),
            );

    let workspace = std::path::Path::new(&session.cwd);
    let workspace = workspace.is_dir().then_some(workspace);

    // 配置快照更新通知：在收集 specs 前调 on_config_updated，使插件先刷新
    // 内部 endpoint/client，再收集 tool_specs——保证 specs 基于最新配置。
    for plugin in plugins {
        plugin.on_config_updated(&cfg);
    }

    let is_first_turn = !*session_ready_fired;

    // 收集 tool_specs + 注入 turn 级状态（workspace/trust_mode/feedback_tx）。
    // 首次 turn 额外注册 provider/override/prompt 到共享注册表。
    let mut plugin_specs: Vec<ToolSpec> = Vec::new();
    let mut seen_tool_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for plugin in plugins {
        // 注入 turn 级状态（每 turn 都需要：feedback 绑定新 TurnUsageSink）
        plugin.set_workspace(workspace);
        plugin.set_trust_mode(trust_mode);
        plugin.set_feedback_tx(crate::core::plugin::PluginFeedbackTx::new(
            cmd_tx.clone(),
            ctx.turn_usage_sink().clone(),
        ));

        let specs = plugin.tool_specs();
        // 首次 turn：注册到共享注册表（后续 turn 通过 Arc 复用，不重复注册）
        if is_first_turn {
            let plugin_as_handler: Arc<dyn crate::tool_override::ToolOverrideHandler> =
                plugin.clone();
            for spec in &specs {
                ctx.register_tool_override(&spec.name, plugin_as_handler.clone());
            }
            let plugin_as_prompt: Arc<dyn crate::tool_override::PromptSectionProvider> =
                plugin.clone();
            ctx.register_prompt_section_provider(plugin_as_prompt);
        }

        for spec in specs {
            if seen_tool_names.insert(spec.name.clone()) {
                plugin_specs.push(spec);
            } else {
                tracing::debug!(
                    tool = %spec.name,
                    plugin = %plugin.id(),
                    "跳过与其他插件重名的工具规格（保留先注册者）"
                );
            }
        }
    }

    // 汇总 exec_env 注入给插件（配置可能变更，每 turn 刷新）
    {
        let mut exec_env = std::collections::BTreeMap::new();
        for plugin in plugins {
            for (key, value) in plugin.exec_env() {
                exec_env.insert(key, value);
            }
        }
        for plugin in plugins {
            plugin.set_exec_env(exec_env.clone());
        }
    }

    let injection_spec = crate::core::plugin::injection_tool_spec();
    let mut tools: Vec<ToolSpec> = Vec::new();
    tools.push(injection_spec);
    tools.extend(plugin_specs);
    ctx.tools = tools.clone();

    // 首次 context 构建 + 插件注册完成后触发一次 on_session_ready
    //（此时 workspace / trust_mode / feedback 已注入）；后续 turn 的再配置
    // 由各插件在 on_config_updated 中统一承载。
    if !*session_ready_fired {
        *session_ready_fired = true;
        for plugin in plugins {
            plugin.on_session_ready(session);
        }
    }

    (ctx, tools)
}

/// 从 CoreConfig 快照构建 TurnContext（每 turn 现建）
///
/// `stream_tx` 用于在 LLM 请求重试时发送 `StreamEvent::Retry` 通知。
/// `trust_mode` 是 TiangongCore 持有的会话信任解析句柄，TurnContext 共享它。
fn build_context_from_config(
    config: &crate::core_config::CoreConfig,
    stream_tx: &StdSender<StreamEvent>,
    trust_mode: crate::permission::TrustMode,
    storage_root: std::path::PathBuf,
) -> crate::turn_context::TurnContext {
    use crate::agent_config::AgentConfig;
    use crate::model::OnRetryCallback;
    use crate::turn_context::TurnContext;

    let agent_config = AgentConfig {
        trust_mode: config.trust_mode,
        default_trust_mode: config.default_trust_mode,
        custom_system_prompt: config.custom_system_prompt.clone(),
        reasoning_effort: config.reasoning_effort.clone(),
    };

    // 构造重试回调：发送 StreamEvent::Retry 通知前端
    let retry_tx = stream_tx.clone();
    let on_retry: OnRetryCallback =
        Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
            let _ = retry_tx.send(StreamEvent::Retry {
                message: error_text.to_string(),
                attempt,
                max_attempts,
            });
        });

    // context_limit 由 to_core_config 在加载时解析注入（core 不做配置磁盘 IO）。
    let context_limit = config.context_limit;
    let ctx = TurnContext::new(
        Session::new("placeholder"),
        SingleProviderClient::new(config.llm.chat.clone()).with_on_retry(on_retry.clone()),
        context_limit,
        agent_config,
        trust_mode,
        crate::observe::Observer::new(storage_root),
        Vec::new(),
        MAX_TOOL_ROUNDS,
        MAX_OUTER_ITERATIONS,
        Arc::new(crate::core::plugin::TurnUsageSink::new()),
    );
    ctx
}
