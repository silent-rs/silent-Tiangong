use crate::app::TiangongApp;
use crate::view::*;
use std::path::PathBuf;
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State, Window};

fn ensure_assistant_message(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
) {
    if !session.messages.iter().any(|msg| msg.id == message_id) {
        session.append_message_with_id(
            message_id.to_string(),
            tiangong_core::session::MessageRole::Assistant,
            String::new(),
            String::new(),
        );
    }

    *assistant_msg_id = Some(message_id.to_string());
}

fn append_assistant_delta(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    content: &str,
) {
    if content.trim().is_empty()
        && !session.messages.iter().any(|msg| msg.id == message_id)
    {
        return;
    }
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        if msg.content.trim().is_empty() && content.trim().is_empty() {
            return;
        }
        msg.content.push_str(content);
    }
}

fn append_assistant_reasoning(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    content: &str,
) {
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        msg.reasoning_content.push_str(content);
    }
}

fn cleanup_assistant_before_tool_calls(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
) {
    let Some(message_id) = assistant_msg_id.take() else {
        return;
    };
    let Some(index) = session.messages.iter().position(|msg| {
        msg.id == message_id && msg.role == tiangong_core::session::MessageRole::Assistant
    }) else {
        return;
    };

    let message = &mut session.messages[index];
    if !message.content.trim().is_empty() {
        return;
    }
    message.content.clear();
    if message.reasoning_content.trim().is_empty() && message.media.is_empty() {
        session.messages.remove(index);
    }
}

fn finalize_assistant_tool_calls(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    calls: &[tiangong_types::StreamToolCall],
) {
    if calls.is_empty() {
        cleanup_assistant_before_tool_calls(session, assistant_msg_id);
        return;
    }
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        msg.tool_calls = calls
            .iter()
            .map(|call| tiangong_core::session::MessageToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();
    }
    *assistant_msg_id = None;
}

fn append_tool_result_message(
    session: &mut tiangong_core::session::Session,
    tool_call_id: Option<&str>,
    tool_name: &str,
    content: String,
    is_error: bool,
) {
    let Some(tool_call_id) = tool_call_id else {
        return;
    };
    let mut message =
        tiangong_core::session::Message::new(tiangong_core::session::MessageRole::Tool, content);
    message.tool_call_id = Some(tool_call_id.to_string());
    message.tool_name = Some(tool_name.to_string());
    message.tool_result_is_error = is_error;
    session.messages.push(message);
    session.updated_at = tiangong_core::session::now_text();
}

fn parse_image_markdown_assets(output: &str) -> Vec<tiangong_types::MediaAsset> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("![") || !line.ends_with(')') {
                return None;
            }

            let close_alt = line.find("](")?;
            let alt = line[2..close_alt].trim();
            let url = line[close_alt + 2..line.len() - 1].trim();
            if url.is_empty() {
                return None;
            }

            let mime_type = if url.starts_with("data:image/") {
                url.strip_prefix("data:")
                    .and_then(|raw| raw.split(';').next())
                    .map(str::to_string)
            } else {
                None
            };

            Some(tiangong_types::MediaAsset {
                kind: tiangong_types::MediaKind::Image,
                url: url.to_string(),
                mime_type,
                title: (!alt.is_empty()).then(|| alt.to_string()),
                capability: Some("image_generation".to_string()),
            })
        })
        .collect()
}

fn looks_like_pure_image_markdown(output: &str) -> bool {
    let trimmed = output.trim();
    !trimmed.is_empty()
        && trimmed.lines().all(|line| {
            let line = line.trim();
            line.is_empty()
                || (line.starts_with("![") && line.contains("](") && line.ends_with(')'))
        })
}

fn is_video_tool(name: &str) -> bool {
    matches!(name, "generate_video" | "query_video_generation")
}

/// 从视频工具输出中提取视频 URL 作为 MediaAsset。
/// 支持 MCP 工具返回的 JSON（含 "Video URL: ..."）和纯 URL。
fn parse_video_url_assets(output: &str) -> Vec<tiangong_types::MediaAsset> {
    let text = output.trim();
    let mut urls = Vec::new();

    // 尝试从 "Video URL: <url>" 模式提取
    for line in text.lines() {
        if let Some(pos) = line.find("Video URL:") {
            let url = line[pos + "Video URL:".len()..].trim();
            if !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
                let url = url.split_whitespace().next().unwrap_or(url);
                urls.push(url.to_string());
            }
        }
    }

    // 如果没找到，尝试从 JSON 的字段中提取视频 URL
    if urls.is_empty() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            extract_video_urls_from_json(&value, &mut urls);
        }
    }

    urls.into_iter()
        .map(|url| tiangong_types::MediaAsset {
            kind: tiangong_types::MediaKind::Video,
            url,
            mime_type: Some("video/mp4".to_string()),
            title: Some("生成的视频".to_string()),
            capability: Some("video_generation".to_string()),
        })
        .collect()
}

fn extract_video_urls_from_json(value: &serde_json::Value, urls: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if (s.starts_with("http://") || s.starts_with("https://"))
                && (s.contains(".mp4") || s.contains("video"))
            {
                urls.push(s.clone());
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                extract_video_urls_from_json(v, urls);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                extract_video_urls_from_json(v, urls);
            }
        }
        _ => {}
    }
}

fn append_assistant_media(
    session: &mut tiangong_core::session::Session,
    media: Vec<tiangong_types::MediaAsset>,
) {
    session.append_message_with_media(
        tiangong_core::session::MessageRole::Assistant,
        String::new(),
        media,
    );
}

fn usage_delta(
    total: &tiangong_types::TokenUsage,
    already_recorded: &tiangong_types::TokenUsage,
) -> tiangong_types::TokenUsage {
    tiangong_types::TokenUsage {
        prompt_tokens: total
            .prompt_tokens
            .saturating_sub(already_recorded.prompt_tokens),
        completion_tokens: total
            .completion_tokens
            .saturating_sub(already_recorded.completion_tokens),
        total_tokens: total
            .total_tokens
            .saturating_sub(already_recorded.total_tokens),
    }
}

fn add_session_usage(
    session: &mut tiangong_core::session::Session,
    usage: &tiangong_types::TokenUsage,
) {
    if usage.total_tokens == 0 {
        return;
    }
    session.token_usage.accumulate(usage);
    session.updated_at = tiangong_core::session::now_text();
}

fn parse_model_capability(
    capability: &str,
) -> Result<tiangong_core::models_config::ModelCapability, String> {
    tiangong_core::models_config::ModelCapability::from_key(capability)
        .ok_or_else(|| format!("不支持的能力类型：{capability}"))
}

fn has_capability_in_state(
    core_state: &tiangong_core::app_state::TiangongState,
    capability: tiangong_core::models_config::ModelCapability,
) -> bool {
    core_state.models_config().has_capability(capability)
}

// ============================================================================
// 辅助函数：构建完整的 RunSnapshot
// ============================================================================

fn build_full_snapshot_with_status(
    core_state: &tiangong_core::app_state::TiangongState,
    is_executing: bool,
) -> RunSnapshotView {
    let sid = core_state.active_session_id();
    build_session_snapshot(core_state, sid, is_executing)
}

fn build_session_snapshot(
    core_state: &tiangong_core::app_state::TiangongState,
    session_id: &str,
    is_session_executing: bool,
) -> RunSnapshotView {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.input_draft().to_string();

    let selected_session = core_state.sessions().iter().find(|s| s.id == session_id);

    let messages: Vec<tiangong_types::Message> = selected_session
        .map(|s| s.messages.clone())
        .unwrap_or_default();

    let current_plan = core_state
        .active_task_plans()
        .first()
        .map(TaskPlan::from_session_task_plan);

    let pending_session_ids = core_state.pending_session_ids();

    let mut snapshot = RunSnapshotView::from_core_with_session(
        core_snapshot,
        messages,
        input_draft,
        current_plan,
        pending_session_ids,
    );
    snapshot.last_usage = selected_session.and_then(|session| {
        let usage = session.total_usage();
        (usage.total_tokens > 0).then_some(usage)
    });

    // 按 session 独立判断状态
    if is_session_executing {
        // 该 session 有活跃的 TiangongCore
        if snapshot.last_session_id.as_deref() != Some(session_id) {
            snapshot.status = tiangong_types::RunStatus::Executing;
            snapshot.summary = "正在处理".to_string();
        }
    } else {
        // 该 session 没有活跃 core → idle
        snapshot.status = tiangong_types::RunStatus::Idle;
        snapshot.current_plan = None;
    }

    snapshot
}

// ============================================================================
// 会话管理
// ============================================================================

/// 获取所有会话列表
#[tauri::command]
pub fn get_sessions(state: State<TiangongApp>) -> Result<Vec<SessionListItem>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .sessions()
            .iter()
            .map(SessionListItem::from_core)
            .collect())
    })
}

/// 创建新会话
#[tauri::command]
pub fn create_session(state: State<TiangongApp>) -> Result<SessionListItem, String> {
    state.with_state(|core_state| {
        core_state.create_session();
        // 返回新创建的活动会话
        core_state
            .active_session()
            .map(SessionListItem::from_core)
            .ok_or_else(|| anyhow::anyhow!("Failed to create session"))
    })
}

/// 切换到指定会话
#[tauri::command]
pub fn switch_session(session_id: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.switch_session(&session_id);
        Ok(())
    })
}

/// 删除当前会话
#[tauri::command]
pub fn delete_session(state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| core_state.delete_active_session())
}

/// 更新会话标题
#[tauri::command]
pub fn update_session_title(title: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.update_session_title_draft(title);
        core_state.save_active_session_title()
    })
}

// ============================================================================
// 消息和执行
// ============================================================================

/// 发送消息并执行
#[tauri::command]
pub fn send_message(
    content: String,
    app: AppHandle,
    _window: Window,
    state: State<TiangongApp>,
) -> Result<(), String> {
    use std::sync::mpsc;
    use tiangong_types::{SessionStreamEvent, StreamEvent};

    // 准备 session
    let (session_id, user_message_id, session_snapshot) = state
        .with_state(|core_state| core_state.prepare_active_user_message_ingress(content.clone()))?;

    // 获取或创建 TiangongCore
    let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
    let (sid, is_new_core) = state.ensure_core(&session_id, session_snapshot, stream_tx);
    // 发送消息（core 内部会 append 到 core session 并推送 UserMessage 事件）
    {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(&sid) {
            if !core.send_message_with_id(content.clone(), user_message_id) {
                return Err("会话 core 已停止，请重试发送".to_string());
            }
        }
    }

    // 只在新创建 core 时启动消费线程（复用 core 时旧消费线程仍在运行）
    if !is_new_core {
        return Ok(());
    }

    // 消费 SessionStreamEvent：emit 给前端 + 更新 RunStatus + Done 时同步 session
    let app_clone = app.clone();
    thread::spawn(move || {
        let mut assistant_msg_id: Option<String> = None;
        let mut last_tool_args_summary = String::new();
        let mut pending_final_media: Option<Vec<tiangong_types::MediaAsset>> = None;
        let mut recorded_turn_usage = tiangong_types::TokenUsage::default();
        for session_event in stream_rx.iter() {
            // 先 emit 完整事件给前端，再解构
            let _ = app_clone.emit("stream_event", &session_event);

            let sid = session_event.session_id;
            let event = session_event.event;
            let is_done = matches!(event, StreamEvent::Done { .. });
            let is_error = matches!(event, StreamEvent::Error { .. });

            // 更新 session + RunStatus/usage
            let _ = app_clone.state::<TiangongApp>().with_state(|core_state| {
                if let Some(session) = core_state.sessions_mut().iter_mut().find(|s| s.id == sid) {
                    match &event {
                        StreamEvent::UserMessage {
                            message_id,
                            content,
                        } => {
                            recorded_turn_usage = tiangong_types::TokenUsage::default();
                            // Core 已记录用户消息，同步到 TiangongState session
                            if !session.messages.iter().any(|msg| msg.id == *message_id) {
                                session.append_message_with_id(
                                    message_id.clone(),
                                    tiangong_core::session::MessageRole::User,
                                    content.clone(),
                                    String::new(),
                                );
                            }
                        }
                        StreamEvent::Delta {
                            message_id,
                            content,
                        } => {
                            append_assistant_delta(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                content,
                            );
                        }
                        StreamEvent::Reasoning {
                            message_id,
                            content,
                        } => {
                            append_assistant_reasoning(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                content,
                            );
                        }
                        StreamEvent::ToolCalls {
                            message_id,
                            names,
                            calls,
                            usage,
                        } => {
                            finalize_assistant_tool_calls(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                calls,
                            );
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("LLM 输出\ntool_calls: {}", names.join(", ")),
                            );
                            if let Some(usage) = usage {
                                add_session_usage(session, usage);
                                recorded_turn_usage.accumulate(usage);
                            }
                        }
                        StreamEvent::ToolStart {
                            ref args_summary, ..
                        } => {
                            // 不清除 pending_final_media，允许多轮工具调用后仍保留已生成的媒体
                            last_tool_args_summary = args_summary.clone();
                        }
                        StreamEvent::ToolResult {
                            ref name,
                            ref tool_call_id,
                            ok,
                            ref output,
                            ref full_output,
                        } => {
                            let persisted_output = full_output.as_deref().unwrap_or(output);
                            let status = if *ok { "ok=true" } else { "ok=false" };
                            let media_assets = if *ok
                                && name == "generate_image"
                                && looks_like_pure_image_markdown(persisted_output)
                            {
                                parse_image_markdown_assets(persisted_output)
                            } else if *ok && is_video_tool(name) {
                                parse_video_url_assets(persisted_output)
                            } else {
                                Vec::new()
                            };

                            let mut lines = vec![format!("工具执行 [{name}]")];
                            if !last_tool_args_summary.is_empty() {
                                lines.push(format!("命令: {last_tool_args_summary}"));
                            }
                            lines.push(format!("{status} exit_code=0"));
                            lines.push(format!("summary: {name}"));
                            if !media_assets.is_empty() {
                                let media_desc = media_assets
                                    .iter()
                                    .map(|a| match a.kind {
                                        tiangong_types::MediaKind::Image => "图片",
                                        tiangong_types::MediaKind::Video => "视频",
                                        tiangong_types::MediaKind::Audio => "音频",
                                        _ => "文件",
                                    })
                                    .next()
                                    .unwrap_or("媒体");
                                let count = media_assets.len();
                                lines.push(format!("stdout: 已生成 {count} 个{media_desc}"));
                                pending_final_media
                                    .get_or_insert_with(Vec::new)
                                    .extend(media_assets);
                            } else if !persisted_output.trim().is_empty() {
                                lines.push(format!("stdout:\n{persisted_output}"));
                            }
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                lines.join("\n"),
                            );
                            append_tool_result_message(
                                session,
                                tool_call_id.as_deref(),
                                name,
                                persisted_output.to_string(),
                                !*ok,
                            );
                            last_tool_args_summary.clear();
                        }
                        StreamEvent::ApprovalNeeded { .. } => {
                            // 审批请求不写入 session（前端通过 RunStatus 展示审批 UI）
                        }
                        StreamEvent::Error { ref message } => {
                            // 错误前先保存已生成的媒体资源
                            if let Some(media) = pending_final_media.take() {
                                append_assistant_media(session, media);
                            }
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[错误] {message}"),
                            );
                            assistant_msg_id = None;
                        }
                        StreamEvent::Retry {
                            ref message,
                            attempt,
                            max_attempts,
                        } => {
                            let _ = (message, attempt, max_attempts);
                        }
                        StreamEvent::Done { usage } => {
                            if let Some(usage) = usage {
                                let delta = usage_delta(usage, &recorded_turn_usage);
                                add_session_usage(session, &delta);
                                recorded_turn_usage = tiangong_types::TokenUsage::default();
                            }
                            if let Some(media) = pending_final_media.take() {
                                append_assistant_media(session, media);
                            }
                            assistant_msg_id = None;
                        }
                        StreamEvent::MemoryRecallStart { ref strategy } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[记忆检索] 策略: {strategy}"),
                            );
                        }
                        StreamEvent::MemoryRecallDone {
                            hit_count,
                            ref hits,
                        } => {
                            if *hit_count == 0 {
                                session.append_message(
                                    tiangong_core::session::MessageRole::System,
                                    "[记忆检索] 无相关记忆".to_string(),
                                );
                            } else {
                                let items: Vec<String> = hits
                                    .iter()
                                    .map(|h| {
                                        format!("- [{:.2}] {}: {}", h.score, h.title, h.summary)
                                    })
                                    .collect();
                                session.append_message(
                                    tiangong_core::session::MessageRole::System,
                                    format!(
                                        "[记忆检索] 命中 {} 条\n{}",
                                        hit_count,
                                        items.join("\n")
                                    ),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                // RunStatus/usage 更新
                match &event {
                    StreamEvent::ApprovalNeeded {
                        ref request_id,
                        ref tool_name,
                        ref args_summary,
                    } => {
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::WaitingApproval;
                        core_state.store.runtime.run.summary = if args_summary.is_empty() {
                            format!("工具 {tool_name} 需要确认")
                        } else {
                            format!("{tool_name}: {args_summary}")
                        };
                        core_state.store.runtime.run.approval_request_id = Some(request_id.clone());
                    }
                    StreamEvent::ToolStart {
                        name,
                        ref args_summary,
                    } => {
                        // 审批通过后恢复执行状态
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::Executing;
                        core_state.store.runtime.run.approval_request_id = None;
                        core_state.store.runtime.run.summary = if args_summary.is_empty() {
                            format!("正在执行：{name}")
                        } else {
                            format!("正在执行：{name} {args_summary}")
                        };
                    }
                    StreamEvent::ToolResult { name, ok, .. } => {
                        let s = if *ok { "✓" } else { "✗" };
                        core_state.store.runtime.run.summary = format!("{s} {name}");
                    }
                    StreamEvent::ToolCalls { names, usage, .. } => {
                        core_state.store.runtime.run.summary =
                            format!("正在执行：{}", names.join(", "));
                        if usage.is_some() {
                            if let Some(session) =
                                core_state.sessions().iter().find(|s| s.id == sid)
                            {
                                let total = session.total_usage();
                                core_state.store.runtime.run.last_usage =
                                    (total.total_tokens > 0).then_some(total);
                            }
                        }
                    }
                    StreamEvent::Done { ref usage } => {
                        if usage.is_some() {
                            if let Some(session) =
                                core_state.sessions().iter().find(|s| s.id == sid)
                            {
                                let total = session.total_usage();
                                core_state.store.runtime.run.last_usage =
                                    (total.total_tokens > 0).then_some(total);
                            }
                        }
                        core_state.report_run_idle(format!(
                            "模型供应商：{}",
                            core_state.provider_label()
                        ));
                    }
                    StreamEvent::Error { ref message } => {
                        recorded_turn_usage = tiangong_types::TokenUsage::default();
                        core_state.report_run_idle(format!("执行失败：{message}"));
                    }
                    StreamEvent::Retry {
                        attempt,
                        max_attempts,
                        ..
                    } => {
                        core_state.store.runtime.run.summary =
                            format!("重试中 ({attempt}/{max_attempts})...");
                    }
                    StreamEvent::Reasoning { .. } => {
                        core_state.store.runtime.run.summary = "正在思考...".to_string();
                    }
                    StreamEvent::Delta { .. } => {
                        core_state.store.runtime.run.summary = "正在回复...".to_string();
                    }
                    StreamEvent::MemoryRecallStart { .. } => {
                        core_state.store.runtime.run.summary = "正在检索记忆...".to_string();
                    }
                    StreamEvent::MemoryRecallDone { hit_count, .. } => {
                        if *hit_count > 0 {
                            core_state.store.runtime.run.summary =
                                format!("记忆检索完成，命中 {hit_count} 条");
                        } else {
                            core_state.store.runtime.run.summary =
                                "记忆检索完成，无相关记忆".to_string();
                        }
                    }
                    _ => {}
                }
                Ok(())
            });

            // emit run_snapshot
            {
                let is_exec = !is_done && !is_error;
                if let Ok(snapshot) = app_clone
                    .state::<TiangongApp>()
                    .with_state_read(|s| Ok(build_full_snapshot_with_status(s, is_exec)))
                {
                    let _ = app_clone.emit("run_snapshot", &snapshot);
                }
            }

            if is_done || is_error {
                // Done：先持久化，再异步生成标题（不阻塞消费线程）
                let final_sid = sid.clone();

                // 提取标题生成所需数据（在锁内完成，避免长时间持锁）
                let title_task = app_clone.state::<TiangongApp>().with_state(|core_state| {
                    let _ = core_state.persist_session_and_app(&final_sid);
                    let snapshot = build_full_snapshot_with_status(core_state, false);
                    let _ = app_clone.emit("run_snapshot", &snapshot);
                    let _ = app_clone.emit("sessions_updated", &());

                    // 检查是否需要生成标题
                    if let Some(session) = core_state.sessions().iter().find(|s| s.id == final_sid)
                    {
                        let is_default =
                            session.title == "新对话" || session.title.starts_with("会话 ");
                        if is_default {
                            if let Some(input) = session
                                .messages
                                .iter()
                                .find(|m| m.role == tiangong_core::session::MessageRole::User)
                                .map(|m| m.content.clone())
                            {
                                let provider_config = core_state
                                    .store
                                    .provider
                                    .models_config
                                    .to_lite_provider_config();
                                return Ok(Some((input, provider_config)));
                            }
                        }
                    }
                    Ok(None)
                });

                // 异步生成标题（不阻塞消费线程）
                if let Ok(Some((input, provider_config))) = title_task {
                    let app_for_title = app_clone.clone();
                    let sid_for_title = final_sid.clone();
                    thread::spawn(move || {
                        let client =
                            tiangong_core::model::SingleProviderClient::new(provider_config);
                        if let Ok(t) = client.complete_lite(&input) {
                            let clean = t.trim().trim_matches('"').to_string();
                            if !clean.is_empty() {
                                let _ =
                                    app_for_title
                                        .state::<TiangongApp>()
                                        .with_state(|core_state| {
                                            if let Some(s) = core_state
                                                .sessions_mut()
                                                .iter_mut()
                                                .find(|s| s.id == sid_for_title)
                                            {
                                                s.title = clean;
                                                s.updated_at = tiangong_core::session::now_text();
                                            }
                                            let _ =
                                                core_state.persist_session_and_app(&sid_for_title);
                                            let snapshot =
                                                build_full_snapshot_with_status(core_state, false);
                                            let _ = app_for_title.emit("run_snapshot", &snapshot);
                                            let _ = app_for_title.emit("sessions_updated", &());
                                            Ok(())
                                        });
                            }
                        }
                    });
                }
                // 不 break — 消费线程继续运行，等待下一轮消息的 StreamEvent
            }
        }
    });

    Ok(())
}

/// 取消当前执行
#[tauri::command]
pub fn cancel_turn(state: State<TiangongApp>) -> Result<bool, String> {
    let session_id =
        state.with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))?;
    state.cancel_core(&session_id);
    Ok(true)
}

/// 向正在执行的 turn 追加用户消息
#[tauri::command]
pub fn append_message(
    session_id: String,
    content: String,
    app: AppHandle,
    state: State<TiangongApp>,
) -> Result<bool, String> {
    if session_id.trim().is_empty() {
        return Err("当前会话 ID 不能为空".to_string());
    }

    let message_id = scru128::new().to_string();
    if !state.send_to_core_with_id(&session_id, content.clone(), Some(message_id.clone())) {
        let snapshot = state.with_state(|core_state| {
            core_state.report_run_idle("当前会话任务已结束，请重新发送");
            Ok(build_session_snapshot(core_state, &session_id, false))
        })?;
        let _ = app.emit("run_snapshot", &snapshot);
        return Ok(false);
    }

    let snapshot = state.with_state(|core_state| {
        {
            let Some(session) = core_state
                .sessions_mut()
                .iter_mut()
                .find(|session| session.id == session_id)
            else {
                return Err(anyhow::anyhow!("当前会话不存在"));
            };
            if !session.messages.iter().any(|msg| msg.id == message_id) {
                session.append_message_with_id(
                    message_id,
                    tiangong_core::session::MessageRole::User,
                    content,
                    String::new(),
                );
            }
        }

        let usage = core_state
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.total_usage())
            .unwrap_or_default();
        core_state.store.session.input_draft.clear();
        core_state.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        core_state.store.runtime.run.summary = "正在处理".to_string();
        core_state.store.runtime.run.last_session_id = Some(session_id.clone());
        core_state.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        core_state.store.runtime.run.updated_at = tiangong_core::session::now_text();
        core_state.persist_session_and_app(&session_id)?;
        Ok(build_session_snapshot(core_state, &session_id, true))
    })?;
    let _ = app.emit("run_snapshot", &snapshot);

    Ok(true)
}

/// 响应工具审批请求
#[tauri::command]
pub fn respond_approval(
    request_id: String,
    approved: bool,
    state: State<TiangongApp>,
) -> Result<bool, String> {
    let session_id =
        state.with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))?;
    state.respond_approval_to_core(&session_id, request_id, approved);
    Ok(true)
}

/// 获取当前信任模式
#[tauri::command]
pub fn get_trust_mode(state: State<TiangongApp>) -> Result<String, String> {
    state.with_state_read(|core_state| {
        let mode = core_state.agent_config().trust_mode;
        Ok(serde_json::to_value(mode)
            .unwrap_or_default()
            .as_str()
            .unwrap_or("full_trust")
            .to_string())
    })
}

/// 设置信任模式
#[tauri::command]
pub fn set_trust_mode(mode: String, state: State<TiangongApp>) -> Result<(), String> {
    let trust_mode: tiangong_core::permission::TrustMode =
        serde_json::from_value(serde_json::Value::String(mode))
            .map_err(|e| format!("无效的信任模式: {e}"))?;

    // 更新 TiangongState（持久化）
    state.with_state(|core_state| core_state.set_trust_mode(trust_mode))?;
    state.sync_core_config_from_state()?;

    // 只更新当前活跃会话的 core（session 级别）
    let session_id =
        state.with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))?;
    state.set_core_trust_mode(&session_id, trust_mode);

    Ok(())
}

/// 获取会话成本统计
#[tauri::command]
pub fn get_session_cost(
    session_id: Option<String>,
    state: State<TiangongApp>,
) -> Result<serde_json::Value, String> {
    state.with_state_read(|core_state| {
        let sid = session_id
            .as_deref()
            .unwrap_or_else(|| core_state.active_session_id());
        let session = core_state.sessions().iter().find(|s| s.id == sid);
        match session {
            Some(s) => {
                let cost =
                    tiangong_core::observe::build_session_cost(s.id.clone(), &s.task_records);
                Ok(serde_json::to_value(cost).unwrap_or_default())
            }
            None => Ok(serde_json::json!({})),
        }
    })
}

/// 获取当前活跃的 Worker 列表
#[tauri::command]
pub fn list_workers(state: State<TiangongApp>) -> Result<Vec<serde_json::Value>, String> {
    state.with_state_read(|core_state| Ok(core_state.list_active_workers()))
}

/// 获取后台任务列表
#[tauri::command]
pub fn get_background_tasks() -> Result<Vec<serde_json::Value>, String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    let tasks = guard.list();
    tasks
        .into_iter()
        .map(|t| serde_json::to_value(t).map_err(|e| e.to_string()))
        .collect()
}

/// 取消后台任务
#[tauri::command]
pub fn cancel_background_task(task_id: String) -> Result<(), String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    guard.cancel(&task_id);
    Ok(())
}

/// 语音合成：将文本转换为音频，返回 base64 编码的音频数据
#[tauri::command]
pub async fn synthesize_speech(
    text: String,
    state: State<'_, TiangongApp>,
) -> Result<SpeechResult, String> {
    let models_config =
        state.with_state_read(|core_state| Ok(core_state.models_config().clone()))?;
    let output = tiangong_core::media::synthesize_speech(
        &models_config,
        text,
        None,
        None,
        Some("mp3".to_string()),
    )
    .await
    .map_err(|e| e.to_string())?;
    let resp = output.response;

    // 将音频保存到临时文件，通过 asset 协议播放
    let media_dir = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| format!("创建媒体目录失败：{e}"))?;

    let ext = match resp.mime_type.as_str() {
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/opus" => "opus",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        _ => "mp3",
    };
    let file_name = format!("tts_{}.{}", scru128::new(), ext);
    let file_path = media_dir.join(&file_name);
    std::fs::write(&file_path, &resp.audio).map_err(|e| format!("音频文件写入失败：{e}"))?;

    Ok(SpeechResult {
        file_path: file_path.to_string_lossy().to_string(),
        mime_type: resp.mime_type,
    })
}

/// 检查 TTS 能力是否已配置
#[tauri::command]
pub fn has_tts_capability(state: State<TiangongApp>) -> Result<bool, String> {
    has_model_capability("tts".to_string(), state)
}

/// 检查 STT 能力是否已配置
#[tauri::command]
pub fn has_stt_capability(state: State<TiangongApp>) -> Result<bool, String> {
    has_model_capability("stt".to_string(), state)
}

/// 统一的能力可用性查询（基于配置快速检测）
#[tauri::command]
pub fn has_model_capability(capability: String, state: State<TiangongApp>) -> Result<bool, String> {
    let capability = parse_model_capability(&capability)?;
    state.with_state_read(|core_state| Ok(has_capability_in_state(core_state, capability)))
}

/// 获取所有能力的当前配置状态
#[tauri::command]
pub fn get_available_capabilities(
    state: State<TiangongApp>,
) -> Result<Vec<CapabilityAvailabilityInfo>, String> {
    use tiangong_core::models_config::ModelCapability;

    state.with_state_read(|core_state| {
        Ok(ModelCapability::all()
            .iter()
            .map(|capability| CapabilityAvailabilityInfo {
                key: capability.key().to_string(),
                display_name: capability.display_name().to_string(),
                enabled: has_capability_in_state(core_state, *capability),
                routed_model: core_state
                    .models_config()
                    .routed_model(*capability)
                    .map(str::to_string),
            })
            .collect())
    })
}

/// 语音识别：将音频数据转录为文本，同时保存音频文件
#[tauri::command]
pub async fn transcribe_speech(
    audio_base64: String,
    mime_type: String,
    state: State<'_, TiangongApp>,
) -> Result<TranscribeResult, String> {
    let models_config =
        state.with_state_read(|core_state| Ok(core_state.models_config().clone()))?;

    // 解码 base64 音频数据
    use base64::Engine;
    let audio = base64::engine::general_purpose::STANDARD
        .decode(&audio_base64)
        .map_err(|e| format!("音频数据解码失败：{e}"))?;

    // 保存音频文件
    let media_dir = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| format!("创建媒体目录失败：{e}"))?;

    let ext = match mime_type.as_str() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        _ => "wav",
    };
    let file_name = format!("stt_{}.{}", scru128::new(), ext);
    let file_path = media_dir.join(&file_name);
    std::fs::write(&file_path, &audio).map_err(|e| format!("音频文件保存失败：{e}"))?;

    let audio_path = file_path.to_string_lossy().to_string();
    let output = tiangong_core::media::transcribe_audio(&models_config, audio, mime_type, None)
        .await
        .map_err(|e| e.to_string())?;

    Ok(TranscribeResult {
        text: output.response.text,
        audio_path,
        duration: output.response.duration,
    })
}

/// 获取 TTS 可用音色列表
#[tauri::command]
pub async fn list_tts_voices(
    state: State<'_, TiangongApp>,
) -> Result<Vec<serde_json::Value>, String> {
    let models_config =
        state.with_state_read(|core_state| Ok(core_state.models_config().clone()))?;
    let voices = tiangong_core::media::list_tts_voices(&models_config)
        .await
        .map_err(|e| e.to_string())?;

    Ok(voices
        .into_iter()
        .map(|v| {
            serde_json::json!({
                "id": v.id,
                "name": v.name,
                "gender": v.gender,
            })
        })
        .collect())
}

/// 播放本地音频文件（使用系统原生播放器）
#[tauri::command]
pub async fn play_audio_file(file_path: String) -> Result<(), String> {
    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("音频文件不存在：{file_path}"));
    }

    #[cfg(target_os = "macos")]
    {
        tokio::process::Command::new("afplay")
            .arg(&file_path)
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    #[cfg(target_os = "windows")]
    {
        tokio::process::Command::new("powershell")
            .args([
                "-c",
                &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", file_path),
            ])
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        tokio::process::Command::new("aplay")
            .arg(&file_path)
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    Ok(())
}

/// 停止当前正在播放的音频
#[tauri::command]
pub async fn stop_audio() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = tokio::process::Command::new("killall")
            .arg("afplay")
            .output()
            .await;
    }
    Ok(())
}

/// 获取 @提及补全候选列表（已启用的 Skill 和 MCP 服务器）
#[tauri::command]
pub fn get_mention_candidates(state: State<TiangongApp>) -> Result<Vec<MentionCandidate>, String> {
    state.with_state_read(|core_state| {
        let mut candidates = Vec::new();

        // 已启用的 Skill
        for skill in core_state.installed_skills() {
            if skill.enabled {
                candidates.push(MentionCandidate {
                    value: format!("@skill:{}", skill.id),
                    label: skill.name.clone(),
                    kind: "skill".to_string(),
                    hint: if skill.description.is_empty() {
                        format!("v{}", skill.version)
                    } else {
                        skill.description.clone()
                    },
                });
            }
        }

        // 已启用的 MCP 服务器
        let active_tools = tiangong_core::mcp::cached_active_tools();
        for server in core_state.mcp_servers() {
            if server.enabled {
                let tool_count = active_tools
                    .iter()
                    .find(|(name, _)| name == &server.name)
                    .map(|(_, tools)| tools.len())
                    .unwrap_or(0);
                candidates.push(MentionCandidate {
                    value: format!("@mcp:{}", server.name),
                    label: server.name.clone(),
                    kind: "mcp".to_string(),
                    hint: format!("{} 工具", tool_count),
                });
            }
        }

        Ok(candidates)
    })
}

/// 获取运行状态快照
#[tauri::command]
pub fn get_run_snapshot(state: State<TiangongApp>) -> Result<RunSnapshotView, String> {
    let active_id = state.with_state_read(|s| Ok(s.active_session_id().to_string()))?;
    let is_exec = state.is_session_executing(&active_id);
    state.with_state_read(|core_state| Ok(build_full_snapshot_with_status(core_state, is_exec)))
}

/// 获取输入草稿
#[tauri::command]
pub fn get_input_draft(state: State<TiangongApp>) -> Result<String, String> {
    state.with_state_read(|core_state| Ok(core_state.input_draft().to_string()))
}

/// 设置输入草稿
#[tauri::command]
pub fn set_input_draft(content: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.update_draft(content);
        Ok(())
    })
}

/// 获取活动会话的工作目录
#[tauri::command]
pub fn get_session_cwd(state: State<TiangongApp>) -> Result<String, String> {
    state.with_state_read(|core_state| Ok(core_state.active_session_cwd().to_string()))
}

/// 设置活动会话的工作目录
#[tauri::command]
pub fn set_session_cwd(cwd: String, state: State<TiangongApp>) -> Result<(), String> {
    // 验证路径存在且是目录
    let path = std::path::Path::new(&cwd);
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{cwd}"));
    }
    state.with_state(|core_state| core_state.update_active_session_cwd(cwd))
}

// ============================================================================
// MCP 管理
// ============================================================================

/// 获取 MCP 服务器列表
#[tauri::command]
pub fn get_mcp_servers(state: State<TiangongApp>) -> Result<Vec<McpServerView>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .mcp_servers()
            .iter()
            .map(McpServerView::from_core)
            .collect())
    })
}

/// 获取 MCP 服务器健康状态
#[tauri::command]
pub fn get_mcp_health() -> Result<Vec<serde_json::Value>, String> {
    let statuses = tiangong_core::mcp::mcp_server_health_statuses();
    statuses
        .into_iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect()
}

/// 注册 MCP 服务器
#[tauri::command]
pub fn register_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    env: Option<std::collections::HashMap<String, String>>,
    state: State<TiangongApp>,
) -> Result<String, String> {
    use tiangong_core::app_state::RegisterMcpServerOptions;
    use tiangong_core::app_state::RegisterMcpServerRequest;

    let message = state.with_state(|core_state| {
        // 转换 env HashMap 为 Vec<(String, String)>
        let env_vec = env.unwrap_or_default().into_iter().collect();

        let request = RegisterMcpServerRequest {
            name: name.clone(),
            command,
            args,
            tags: vec![],
            enabled: true,
            options: RegisterMcpServerOptions {
                env: env_vec,
                ..Default::default()
            },
        };
        core_state.register_mcp_server(request)
    })?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 移除 MCP 服务器
#[tauri::command]
pub fn remove_mcp_server(name: String, state: State<TiangongApp>) -> Result<String, String> {
    let message = state.with_state(|core_state| core_state.remove_mcp_server(&name))?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 设置 MCP 服务器启用状态
#[tauri::command]
pub fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    state: State<TiangongApp>,
) -> Result<String, String> {
    let message =
        state.with_state(|core_state| core_state.set_mcp_server_enabled(&name, enabled))?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

// ============================================================================
// Skill 管理
// ============================================================================

/// 获取已安装的 Skill 列表
#[tauri::command]
pub fn get_skills(state: State<TiangongApp>) -> Result<Vec<SkillView>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .installed_skills()
            .iter()
            .map(SkillView::from_core)
            .collect())
    })
}

/// 刷新 Skill 注册表（重扫 skills/<id>/）
#[tauri::command]
pub fn refresh_skills(state: State<TiangongApp>) -> Result<String, String> {
    let message = state.with_state(|core_state| core_state.refresh_skills())?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 检测或清理孤儿 Skill 托管 MCP 配置
#[tauri::command]
pub fn gc_skills(apply: bool, state: State<TiangongApp>) -> Result<String, String> {
    let message = state.with_state(|core_state| core_state.gc_skills(apply))?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 获取 Skill 完整详情（按需读取 SKILL.md）
#[tauri::command]
pub fn get_skill_detail(id: String, state: State<TiangongApp>) -> Result<SkillDetailView, String> {
    state.with_state_read(|core_state| {
        let detail = core_state.get_skill_detail(&id)?;
        Ok(SkillDetailView::from_core(&detail))
    })
}

/// 检查 Skill 安装需求（返回需要配置的环境变量列表）
#[tauri::command]
pub fn inspect_skill(path: String, state: State<TiangongApp>) -> Result<SkillInspection, String> {
    state.with_state_read(|core_state| {
        let inspection = core_state.inspect_skill_install_requirements(&path, true)?;
        Ok(SkillInspection {
            env_vars: inspection.env_vars,
            missing_env_vars: inspection.missing_env_vars,
            dependencies: inspection.dependencies,
        })
    })
}

/// 安装 Skill（支持传入环境变量配置）
#[tauri::command]
pub fn install_skill(
    path: String,
    env_values: Option<std::collections::HashMap<String, String>>,
    state: State<TiangongApp>,
) -> Result<String, String> {
    let message = state.with_state(|core_state| {
        let env: Vec<(String, String)> = env_values
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .collect();
        core_state.install_local_skill_with_options_and_inputs(&path, true, true, &env)
    })?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 移除 Skill
#[tauri::command]
pub fn remove_skill(id: String, state: State<TiangongApp>) -> Result<String, String> {
    let message = state.with_state(|core_state| core_state.remove_skill(&id))?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

/// 获取 Skill 的环境变量（合并 skill.toml 声明的 requires.env + .env.local 已有值）
#[tauri::command]
pub fn get_skill_env(
    id: String,
    state: State<TiangongApp>,
) -> Result<std::collections::HashMap<String, String>, String> {
    state.with_state_read(|core_state| {
        let skill = core_state
            .installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;

        let skill_dir = std::path::Path::new(&skill.source.value);
        let mut env = std::collections::HashMap::new();

        // 1. 从 skill.toml 的 requires.env 读取声明的 key（值为空）
        let toml_path = skill_dir.join("skill.toml");
        if let Ok(raw) = std::fs::read_to_string(&toml_path) {
            #[derive(serde::Deserialize, Default)]
            struct T {
                #[serde(default)]
                requires: R,
            }
            #[derive(serde::Deserialize, Default)]
            struct R {
                #[serde(default)]
                env: Vec<String>,
            }
            if let Ok(parsed) = toml::from_str::<T>(&raw) {
                for key in parsed.requires.env {
                    env.insert(key, String::new());
                }
            }
        }

        // 2. 从 .env.local 读取已有值（覆盖空值）
        let env_path = skill_dir.join(".env.local");
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    env.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        Ok(env)
    })
}

/// 设置 Skill 的环境变量
#[tauri::command]
pub fn set_skill_env(
    id: String,
    env: std::collections::HashMap<String, String>,
    state: State<TiangongApp>,
) -> Result<(), String> {
    state.with_state_read(|core_state| {
        let skill = core_state
            .installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;
        let env_path = std::path::Path::new(&skill.source.value).join(".env.local");
        let lines: Vec<String> = env
            .iter()
            .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
            .map(|(k, v)| format!("{}={}", k.trim(), v.trim()))
            .collect();
        if lines.is_empty() {
            let _ = std::fs::remove_file(&env_path);
        } else {
            std::fs::write(&env_path, format!("{}\n", lines.join("\n")))
                .map_err(|e| anyhow::anyhow!("写入 .env.local 失败：{e}"))?;
        }
        Ok(())
    })
}

/// 设置 Skill 启用状态
#[tauri::command]
pub fn set_skill_enabled(
    id: String,
    enabled: bool,
    state: State<TiangongApp>,
) -> Result<String, String> {
    let message = state.with_state(|core_state| core_state.set_skill_enabled(&id, enabled))?;
    state.sync_core_config_from_state()?;
    Ok(message)
}

// ============================================================================
// Server 管理
// ============================================================================

/// 获取 Server 配置
#[tauri::command]
pub fn get_server_config() -> Result<ServerConfigView, String> {
    let config = tiangong_server::config::load_server_config();
    let running = is_server_running();
    let auth_token_masked = config.masked_auth_token();
    Ok(ServerConfigView {
        host: config.host,
        port: config.port,
        auth_token_masked,
        running,
    })
}

/// 设置 Server 配置
#[tauri::command]
pub fn set_server_config(
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> Result<String, String> {
    let config = tiangong_server::config::ServerConfig {
        host,
        port,
        auth_token,
    };
    tiangong_server::config::save_server_config(&config).map_err(|e| e.to_string())?;
    Ok("Server 配置已保存".to_string())
}

/// 检查 Server 是否在运行（通过 PID 文件判断）
fn is_server_running() -> bool {
    let pid_path = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.pid");
    if !pid_path.exists() {
        return false;
    }
    match std::fs::read_to_string(&pid_path) {
        Ok(pid_str) => {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // 检查进程是否存在
                #[cfg(unix)]
                {
                    use std::process::Command;
                    Command::new("kill")
                        .arg("-0")
                        .arg(pid.to_string())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    let _ = pid;
                    false
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// 获取用户 home 目录
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| v != std::ffi::OsStr::new(""))
    {
        return Some(PathBuf::from(profile));
    }
    None
}

// ============================================================================
// Connector 管理
// ============================================================================

/// 获取 Connector 列表
#[tauri::command]
pub fn get_connectors() -> Result<Vec<ConnectorInfoView>, String> {
    let configs = tiangong_server::config::load_connectors_config();
    Ok(configs.iter().map(ConnectorInfoView::from_config).collect())
}

/// 设置 Connector 启用状态
#[tauri::command]
pub fn set_connector_enabled(name: String, enabled: bool) -> Result<String, String> {
    tiangong_server::config::set_connector_enabled(&name, enabled).map_err(|e| e.to_string())?;
    Ok(format!(
        "Connector \"{}\" 已{}",
        name,
        if enabled { "启用" } else { "禁用" }
    ))
}

// ============================================================================
// 模型配置（Provider + Model + Routing 三层架构）
// ============================================================================

/// 获取模型配置
#[tauri::command]
pub fn get_models_config(state: State<TiangongApp>) -> Result<ModelsConfigView, String> {
    state.with_state_read(|core_state| Ok(ModelsConfigView::from_core(core_state.models_config())))
}

/// 设置模型配置
#[tauri::command]
pub fn set_models_config(
    config: ModelsConfigView,
    state: State<TiangongApp>,
) -> Result<(), String> {
    state.with_state(|core_state| {
        let core_config = config.to_core();
        core_state.save_models_config(core_config)
    })?;
    state.sync_core_config_from_state()?;
    Ok(())
}

/// 获取所有可用的模型能力列表
#[tauri::command]
pub fn get_model_capabilities() -> Result<Vec<ModelCapabilityInfo>, String> {
    use tiangong_core::models_config::ModelCapability;

    let caps = ModelCapability::all()
        .iter()
        .map(|c| {
            let key = serde_json::to_value(c).unwrap_or_default();
            ModelCapabilityInfo {
                key: key.as_str().unwrap_or_default().to_string(),
                display_name: c.display_name().to_string(),
            }
        })
        .collect();
    Ok(caps)
}

/// 获取模型列表
#[tauri::command]
pub fn get_model_list(state: State<TiangongApp>) -> Result<Vec<String>, String> {
    state.with_state_read(|core_state| Ok(core_state.model_list().to_vec()))
}

/// 根据 provider 配置获取该 provider 的可用模型列表
#[tauri::command]
pub fn fetch_provider_models(
    base_url: String,
    api_key: String,
    timeout_ms: Option<u64>,
    protocol: Option<String>,
) -> Result<Vec<String>, String> {
    use tiangong_core::model::{ModelProviderConfig, ProviderProtocol, SingleProviderClient};
    use tiangong_core::models_config::ModelsConfig;

    let resolved_key = ModelsConfig::resolve_api_key(&api_key);
    let config = ModelProviderConfig {
        api_auth_token: resolved_key,
        api_base_url: base_url,
        api_timeout_ms: timeout_ms.unwrap_or(60_000).to_string(),
        api_protocol: protocol
            .as_deref()
            .and_then(|value| value.parse::<ProviderProtocol>().ok())
            .unwrap_or_default(),
        api_model: String::new(),
        api_lite_model: String::new(),
    };
    SingleProviderClient::list_models(&config).map_err(|e| e.to_string())
}

fn embedding_probe_urls(base_url: &str) -> Result<Vec<String>, String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }

    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("/embeddings").unwrap_or(cleaned);
    let primary = format!("{cleaned}/embeddings");

    if cleaned.ends_with("/v1") {
        return Ok(vec![primary]);
    }

    Ok(vec![primary, format!("{cleaned}/v1/embeddings")])
}

/// 探测 OpenAI 兼容 Embedding 接口返回的向量维度
#[tauri::command]
pub async fn probe_embedding_dimension(
    base_url: String,
    api_key: String,
    model: String,
    timeout_ms: Option<u64>,
    protocol: Option<String>,
) -> Result<usize, String> {
    use tiangong_core::models_config::ModelsConfig;

    let protocol = protocol.unwrap_or_else(|| "openai_compatible".to_string());
    if protocol != "openai_compatible" {
        return Err("Embedding 维度探测仅支持 OpenAI 兼容协议".to_string());
    }

    let model = model.trim();
    if model.is_empty() {
        return Err("模型名称不能为空".to_string());
    }

    let urls = embedding_probe_urls(&base_url)?;
    let api_key = ModelsConfig::resolve_api_key(&api_key);
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(60_000));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    let payload = serde_json::json!({
        "model": model,
        "input": "dimension probe",
        "encoding_format": "float",
    });

    let mut last_error = None;
    for url in urls {
        let mut request = client.post(&url).json(&payload);
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(&api_key);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                last_error = Some(format!("请求 Embedding 接口失败：{url}，{err}"));
                continue;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            last_error = Some(format!(
                "Embedding 接口返回错误：HTTP {status}，响应：{body}"
            ));
            continue;
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|err| format!("解析 Embedding 响应失败：{err}"))?;
        let embedding = value
            .pointer("/data/0/embedding")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "Embedding 响应中缺少 data[0].embedding".to_string())?;
        if embedding.is_empty() {
            return Err("Embedding 响应向量为空".to_string());
        }
        return Ok(embedding.len());
    }

    Err(last_error.unwrap_or_else(|| "无法请求 Embedding 接口".to_string()))
}
