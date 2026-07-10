//! CLI REPL — 类似 codex/claude code 风格交互

use anyhow::Result;
use tiangong_app_state::app_state::TiangongState;
use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::TiangongCore;
use tiangong_types::{SessionStreamEvent, StreamEvent};

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::commands;
use crate::completion;
use crate::input::InputReader;
use crate::output;

pub fn run(trust_mode: Option<tiangong_core::permission::TrustMode>) -> Result<()> {
    // 初始化日志：输出到 stderr（不污染 stdout 的流式对话/交互输出）。
    // 默认 warn 级别以上，可用 RUST_LOG 覆盖。多次调用（如测试）安全忽略。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    // 初始化 config 内存单例（从磁盘加载一次）。
    tiangong_config::registry::init();
    let core_config = tiangong_config::registry::config().to_core_config();
    let models = tiangong_config::registry::models();

    let config = tiangong_core::core_config::CoreConfigProvider::new(core_config);

    let mut state = TiangongState::load_or_default();
    // storage_root 由 app-state 统一计算；plugin 由 app 注入同一根目录。
    let storage_root = tiangong_app_state::app_state::storage_root();
    let skill_plugin: std::sync::Arc<tiangong_plugin_skill::SkillPlugin> = std::sync::Arc::new(
        tiangong_plugin_skill::SkillPlugin::with_storage_root(storage_root.join("skills")),
    );
    // MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具（动态 MCP 工具 spec +
    // 执行分发），CLI 侧经 mcp_plugin 做管理（modal 里的 add/remove/toggle、
    // /config set mcp.*、@mcp 补全、skill 删除后的孤儿 MCP 清理）。
    let mcp_plugin: std::sync::Arc<tiangong_plugin_mcp::McpPlugin> = std::sync::Arc::new(
        tiangong_plugin_mcp::McpPlugin::with_storage_root(storage_root.clone()),
    );
    let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
    // 初始化 Memory Handle（入口层负责，构造时注入 memory 插件）。
    // CLI 入口是同步函数，用临时 tokio runtime block_on。
    let memory_handle = tokio::runtime::Runtime::new()
        .map(|rt| {
            rt.block_on(tiangong_memory::registry::init_memory_handle_for_process(
                config.generation(),
                tiangong_memory::ProcessType::Cli,
            ))
        })
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "创建临时 tokio runtime 失败，memory 将不可用");
            None
        });

    let core = TiangongCore::builder()
        .config(config.clone())
        .session(tiangong_core::session::Session::new("新对话"))
        .event_sender(stream_tx)
        .plugins({
            // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
            use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
            let resolve_ep = |cap: ModelCapability| {
                models
                    .resolve_for_capability(cap)
                    .map(ModelEndpoint::from_resolved)
            };
            let mut plugins = tiangong_plugin_fs::default_plugins();
            plugins.extend(tiangong_plugin_index::default_plugins());
            if let Some(ep) = resolve_ep(ModelCapability::ImageGeneration) {
                plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
            }
            if let Some(ep) = resolve_ep(ModelCapability::VideoGeneration) {
                plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
            }
            if let Some(ep) = resolve_ep(ModelCapability::Tts) {
                plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
            }
            if let Some(ep) = resolve_ep(ModelCapability::Stt) {
                plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
            }
            plugins.extend(tiangong_plugin_memory::default_plugins(memory_handle));
            plugins.extend(tiangong_plugin_fetch::default_plugins());
            plugins.extend(tiangong_plugin_command::default_plugins());
            plugins.extend(tiangong_plugin_scheduler::default_plugins());
            plugins.extend(tiangong_plugin_task::default_plugins());
            // analyze-attachment：仅当配置了独立 multimodal 路由、且 chat 非 multimodal 时才注册。
            if models.has_capability(ModelCapability::Multimodal)
                && !models.chat_is_multimodal()
                && let Some(client) =
                    resolve_ep(ModelCapability::Multimodal).map(SingleProviderClient::new)
            {
                plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
            }
            // Skill 插件：dual-ownership——core 拿 clone 做 LLM 工具，
            // CLI 侧经 skill_plugin 做管理（modal 里的 remove/set_enabled）。
            plugins.push(skill_plugin.clone());
            // MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具（动态 MCP 工具），
            // CLI 侧经 mcp_plugin 做管理。
            plugins.push(mcp_plugin.clone());
            plugins
        })
        .storage(tiangong_core::core::CoreStorageLocation::new(storage_root))
        .build()?;

    // CLI --trust-mode 参数覆盖
    if let Some(mode) = trust_mode {
        core.set_trust_mode(mode);
    }
    let mut reader = InputReader::new();
    let mut draft_new_session = true;

    output::welcome();

    loop {
        // 消费残留事件
        while stream_rx.try_recv().is_ok() {}

        // prompt
        let short_id: String = core.session_id().chars().take(8).collect();
        let prompt = format!("\x1b[2m{short_id}\x1b[0m \x1b[1;36m❯\x1b[0m ");

        let input = {
            reader.read_line(&prompt, |buf, cursor| {
                if let Some((trigger, _start, prefix)) = completion::detect_trigger(buf, cursor) {
                    completion::complete(trigger, &prefix, &skill_plugin, &mcp_plugin)
                } else {
                    Vec::new()
                }
            })?
        };

        let input = match input {
            Some(line) => line,
            None => break,
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        reader.push_history(trimmed);

        // / 命令（通过 TiangongState 处理）
        if trimmed.starts_with('/') {
            match commands::handle_command(
                &mut state,
                &config,
                trimmed,
                &mut draft_new_session,
                &skill_plugin,
                &mcp_plugin,
            ) {
                Ok(true) => break,
                Ok(false) => continue,
                Err(err) => {
                    output::error(&format!("{err}"));
                    continue;
                }
            }
        }

        // 首次发送时创建会话
        if draft_new_session {
            state.create_session();
            draft_new_session = false;
        }

        // 发送消息
        if let Err(e) = core.deliver(AgentInputKind::message(trimmed.to_string())) {
            tracing::warn!(error = %e, "消息投递失败（worker 可能已停止）");
        }

        // 处理响应流
        handle_response(&stream_rx, &core);

        output::separator();
    }

    output::status("再见！");
    // 获取 Core 的最终 session 并持久化
    // worker panic 时无法取回会话，记录告警后跳过持久化（避免丢失提示）。
    match core.into_session() {
        Ok(final_session) if !final_session.messages.is_empty() => {
            state.save_core_session(final_session);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "获取最终 session 失败，跳过持久化"),
    }
    tiangong_memory::registry::shutdown_memory_registry_blocking();
    Ok(())
}

/// 处理完整的响应流
fn handle_response(rx: &mpsc::Receiver<SessionStreamEvent>, core: &TiangongCore) {
    let mut state = ResponseState::new();
    let mut last_event_at = Instant::now();
    let timeout = Duration::from_secs(300);
    let poll_interval = Duration::from_millis(50);

    loop {
        let mut had_event = false;

        loop {
            match rx.try_recv() {
                Ok(session_event) => {
                    had_event = true;
                    last_event_at = Instant::now();
                    if state.process(&session_event.event, core) {
                        return;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.finish();
                    output::error("响应流已断开");
                    break;
                }
            }
        }

        if last_event_at.elapsed() >= timeout {
            state.finish();
            output::error("等待响应超时（300s）");
            break;
        }

        if !had_event {
            std::thread::sleep(poll_interval);
        }
    }
}

/// 响应流状态机
struct ResponseState {
    active_stream: ActiveStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveStream {
    Idle,
    Reasoning { message_id: String },
    Assistant { message_id: String },
    Worker { worker_id: String },
}

impl ResponseState {
    fn new() -> Self {
        Self {
            active_stream: ActiveStream::Idle,
        }
    }

    /// 处理单个事件，返回 true 表示本轮结束
    fn process(&mut self, event: &StreamEvent, core: &TiangongCore) -> bool {
        match event {
            StreamEvent::UserMessage { .. } => {
                // CLI 模式下用户消息由 REPL 自己显示，忽略
            }
            StreamEvent::Reasoning {
                message_id,
                content,
            } => {
                self.ensure_reasoning_stream(message_id);
                output::explanation_delta(content);
            }

            StreamEvent::Delta {
                message_id,
                content,
            }
            | StreamEvent::ReactText {
                message_id,
                content,
            }
            | StreamEvent::SummaryText {
                message_id,
                content,
            } => {
                self.ensure_assistant_stream(message_id);
                output::delta(content);
            }

            StreamEvent::PhaseChanged { .. } => {}

            StreamEvent::ToolCalls { names, .. } => {
                self.end_active_stream();
                output::tool_calls(names);
            }

            StreamEvent::ToolStart { name, args_summary } => {
                self.end_active_stream();
                if args_summary.is_empty() {
                    output::tool_start(name);
                } else {
                    output::tool_start(&format!("{name} {args_summary}"));
                }
            }

            StreamEvent::ToolResult {
                name, ok, output, ..
            } => {
                self.end_active_stream();
                output::tool_result(name, *ok, output);
            }

            StreamEvent::TokenUsage { .. } => {}

            StreamEvent::ApprovalNeeded {
                request_id,
                tool_name,
                args_summary,
            } => {
                self.end_active_stream();
                output::approval_needed(tool_name, args_summary);
                // 等待用户输入 y/n
                let approved = loop {
                    eprint!("\x1b[1;33m  允许执行？(y/n): \x1b[0m");
                    let mut buf = String::new();
                    if std::io::stdin().read_line(&mut buf).is_err() {
                        break false;
                    }
                    match buf.trim().to_lowercase().as_str() {
                        "y" | "yes" => break true,
                        "n" | "no" => break false,
                        _ => {
                            eprintln!("  请输入 y 或 n");
                        }
                    }
                };
                if let Err(e) = core.deliver(AgentInputKind::approval(request_id.clone(), approved))
                {
                    tracing::warn!(error = %e, "审批响应投递失败（worker 可能已停止）");
                }
                if approved {
                    output::status("已允许");
                } else {
                    output::warn("已拒绝");
                }
            }

            StreamEvent::Done { .. } => {
                self.finish();
                return true;
            }

            StreamEvent::Error { message } => {
                self.finish();
                output::error(message);
                return true;
            }

            StreamEvent::Retry {
                message,
                attempt,
                max_attempts,
            } => {
                self.end_active_stream();
                output::warn(&format!("重试 ({attempt}/{max_attempts})：{message}"));
            }

            StreamEvent::WorkerStarted {
                worker_id: _,
                worker_label,
            } => {
                self.end_active_stream();
                output::worker_started(worker_label);
            }

            StreamEvent::WorkerChunk {
                worker_id,
                worker_label,
                content,
            } => {
                self.ensure_worker_stream(worker_id, worker_label);
                output::worker_stream_delta(content);
            }

            StreamEvent::WorkerCompleted {
                worker_id: _,
                worker_label,
                success,
            } => {
                self.end_active_stream();
                output::worker_completed(worker_label, *success);
            }

            StreamEvent::MemoryRecallStart { strategy } => {
                self.end_active_stream();
                output::status(&format!("记忆检索 (策略: {strategy})..."));
            }

            StreamEvent::MemoryRecallProgress { phase } => {
                output::status(&format!("记忆检索: {phase}..."));
            }

            StreamEvent::MemoryRecallDone { hit_count, hits } => {
                if *hit_count == 0 {
                    output::status("记忆检索完成，无相关记忆");
                } else {
                    output::status(&format!("记忆检索完成，命中 {hit_count} 条"));
                    for h in hits {
                        output::status(&format!("  [{:.2}] {}: {}", h.score, h.title, h.summary));
                    }
                }
            }

            // ===== 多智能体团队事件 =====
            StreamEvent::AgentCreated { role, label, .. } => {
                self.end_active_stream();
                output::status(&format!("[{role}] {label} 已加入团队"));
            }

            StreamEvent::AgentStatusChanged { label, status, .. } => {
                output::status(&format!("[{label}] 状态: {status}"));
            }

            StreamEvent::AgentNotification {
                agent_label,
                content,
                level,
                ..
            } => {
                self.end_active_stream();
                match level.as_str() {
                    "error" => output::error(&format!("[{agent_label}] {content}")),
                    "warning" => output::warn(&format!("[{agent_label}] {content}")),
                    "question" => output::status(&format!("[{agent_label}] ❓ {content}")),
                    _ => output::status(&format!("[{agent_label}] {content}")),
                }
            }

            StreamEvent::AgentMessage {
                from_agent_label,
                to_agent_label,
                content,
                ..
            } => {
                output::status(&format!(
                    "[{from_agent_label} → {to_agent_label}] {content}"
                ));
            }

            StreamEvent::AgentOutput {
                agent_label,
                messages,
                ..
            } => {
                self.end_active_stream();
                output::status(&format!("[{agent_label}] 输出 {} 条消息", messages.len()));
            }

            StreamEvent::FileLockChanged {
                path,
                holder_agent_label,
                action,
                ..
            } => {
                let holder = holder_agent_label.as_deref().unwrap_or("unknown");
                output::status(&format!("文件锁 {action}: {} (by {holder})", path));
            }

            StreamEvent::ContextCompressing {
                summary_up_to,
                total_messages,
            } => {
                self.end_active_stream();
                output::status(&format!(
                    "正在压缩上下文: 已摘要 {summary_up_to} 条，当前 {total_messages} 条消息"
                ));
            }

            StreamEvent::ContextCompressed {
                action,
                summary_up_to,
                remaining_messages,
            } => {
                self.end_active_stream();
                output::status(&format!(
                    "上下文{}: 已处理 {summary_up_to} 条，剩余 {remaining_messages} 条",
                    action.display_text()
                ));
            }

            StreamEvent::IndexStatus { phase, count } => {
                if phase == "done" {
                    output::status(&format!("索引扫描完成: {count} 个文件"));
                }
            }
        }
        false
    }

    fn ensure_reasoning_stream(&mut self, message_id: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Reasoning {
                message_id: current_message_id
            } if current_message_id == message_id
        ) {
            self.end_active_stream();
            output::explanation_start();
            self.active_stream = ActiveStream::Reasoning {
                message_id: message_id.to_string(),
            };
        }
    }

    fn ensure_assistant_stream(&mut self, message_id: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Assistant {
                message_id: current_message_id
            } if current_message_id == message_id
        ) {
            self.end_active_stream();
            output::assistant_start();
            self.active_stream = ActiveStream::Assistant {
                message_id: message_id.to_string(),
            };
        }
    }

    fn ensure_worker_stream(&mut self, worker_id: &str, worker_label: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Worker {
                worker_id: current_worker_id
            } if current_worker_id == worker_id
        ) {
            self.end_active_stream();
            output::worker_stream_start(worker_label);
            self.active_stream = ActiveStream::Worker {
                worker_id: worker_id.to_string(),
            };
        }
    }

    fn end_active_stream(&mut self) {
        match self.active_stream {
            ActiveStream::Idle => {}
            ActiveStream::Reasoning { .. } => output::explanation_end(),
            ActiveStream::Assistant { .. } => output::delta_end(),
            ActiveStream::Worker { .. } => output::worker_stream_end(),
        }
        self.active_stream = ActiveStream::Idle;
    }

    fn finish(&mut self) {
        self.end_active_stream();
        println!();
    }
}
