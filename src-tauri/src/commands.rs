use crate::app::TiangongApp;
use crate::types::*;
use std::path::PathBuf;
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State, Window};

// ============================================================================
// 辅助函数：构建完整的 RunSnapshot
// ============================================================================

fn build_full_snapshot(core_state: &tiangong_core::app_state::TiangongState) -> RunSnapshot {
    build_session_snapshot(core_state, core_state.active_session_id())
}

fn build_session_snapshot(
    core_state: &tiangong_core::app_state::TiangongState,
    session_id: &str,
) -> RunSnapshot {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.input_draft().to_string();

    // 获取指定会话的消息
    let messages: Vec<Message> = core_state
        .sessions()
        .iter()
        .find(|s| s.id == session_id)
        .map(|s| s.messages.iter().map(Message::from_core).collect())
        .unwrap_or_default();

    // 获取当前执行的计划（从 active_task_plans 获取第一个进行中的计划）
    let current_plan = core_state
        .active_task_plans()
        .first()
        .map(TaskPlan::from_session_task_plan);

    let pending_session_ids = core_state.pending_session_ids();

    let mut snapshot = RunSnapshot::from_core_with_session(
        core_snapshot,
        messages,
        input_draft,
        current_plan,
        pending_session_ids,
    );

    // 按 session 修正 status：如果该 session 没有 pending_turn，状态应为 idle
    if core_state.has_pending_turn_for(session_id) {
        // 该 session 正在运行，但全局 RunSnapshot 可能被其他 session 的事件覆盖
        // 如果 last_session_id 不匹配，给一个合理的默认状态
        if snapshot.last_session_id.as_deref() != Some(session_id) {
            snapshot.status = "executing".to_string();
            snapshot.summary = "正在执行中".to_string();
        }
    } else {
        // 该 session 没有在运行
        snapshot.status = "idle".to_string();
        // 保留 summary 供历史查看，但清除执行中相关的字段
        snapshot.current_plan = None;
    }

    snapshot
}

// ============================================================================
// 会话管理
// ============================================================================

/// 获取所有会话列表
#[tauri::command]
pub fn get_sessions(state: State<TiangongApp>) -> Result<Vec<Session>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .sessions()
            .iter()
            .map(Session::from_core)
            .collect())
    })
}

/// 创建新会话
#[tauri::command]
pub fn create_session(state: State<TiangongApp>) -> Result<Session, String> {
    state.with_state(|core_state| {
        core_state.create_session();
        // 返回新创建的活动会话
        core_state
            .active_session()
            .map(Session::from_core)
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
    use tiangong_core::core::TiangongCore;
    use tiangong_types::StreamEvent;

    // 获取 session_id
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))?;

    // 准备 session：确保有活跃会话，记录用户消息
    let session_snapshot = state.with_state(|core_state| {
        core_state.update_draft(content.clone());
        let idx = core_state.ensure_active_session_index();
        let session = core_state.sessions()[idx].clone();
        // 记录用户消息到 TiangongState 的 session（持久化用）
        core_state.sessions_mut()[idx]
            .append_message(tiangong_core::session::MessageRole::User, content.clone());
        core_state.store.session.input_draft.clear();
        // 更新运行状态
        core_state.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        core_state.store.runtime.run.summary = "正在处理".to_string();
        core_state.store.runtime.run.last_session_id = Some(session.id.clone());
        core_state.store.runtime.run.updated_at = tiangong_core::session::now_text();
        let _ = core_state.persist_session_and_app(&session.id);
        Ok(session)
    })?;

    // 发送初始快照（executing 状态）
    if let Ok(snapshot) = state.with_state_read(|s| Ok(build_full_snapshot(s))) {
        let _ = app.emit("run_snapshot", &snapshot);
    }

    // 创建 TiangongCore 并发送消息
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let core = TiangongCore::with_session(session_snapshot, stream_tx);
    core.send_message(content);

    // 消费 StreamEvent → 实时更新 TiangongState session → emit 快照
    let app_clone = app.clone();
    let session_id_for_thread = session_id.clone();
    thread::spawn(move || {
        let sid = session_id_for_thread;
        // 追踪当前 assistant 消息 ID（流式追加用）
        let mut assistant_msg_id: Option<String> = None;

        for event in stream_rx.iter() {
            let is_done = matches!(event, StreamEvent::Done { .. });
            let is_error = matches!(event, StreamEvent::Error { .. });

            // emit StreamEvent 给前端
            let _ = app_clone.emit("stream_event", &event);

            // 根据事件类型实时更新 TiangongState 的 session
            let _ = app_clone.state::<TiangongApp>().with_state(|core_state| {
                let session = core_state
                    .store
                    .session
                    .sessions
                    .iter_mut()
                    .find(|s| s.id == sid);
                let Some(session) = session else {
                    return Ok(());
                };

                match &event {
                    StreamEvent::Delta { content } => {
                        if let Some(ref id) = assistant_msg_id {
                            if let Some(msg) = session.messages.iter_mut().find(|m| m.id == *id) {
                                msg.content.push_str(content);
                            }
                        } else {
                            session.append_message(
                                tiangong_core::session::MessageRole::Assistant,
                                String::new(),
                            );
                            if let Some(msg) = session.messages.last_mut() {
                                msg.content.push_str(content);
                                assistant_msg_id = Some(msg.id.clone());
                            }
                        }
                    }
                    StreamEvent::Reasoning { content } => {
                        if let Some(ref id) = assistant_msg_id {
                            if let Some(msg) = session.messages.iter_mut().find(|m| m.id == *id) {
                                msg.reasoning_content.push_str(content);
                            }
                        } else {
                            session.append_message(
                                tiangong_core::session::MessageRole::Assistant,
                                String::new(),
                            );
                            if let Some(msg) = session.messages.last_mut() {
                                msg.reasoning_content.push_str(content);
                                assistant_msg_id = Some(msg.id.clone());
                            }
                        }
                    }
                    StreamEvent::ToolStart { name, .. } => {
                        core_state.store.runtime.run.summary =
                            format!("正在执行：{name}");
                    }
                    StreamEvent::ToolResult { name, ok, output } => {
                        let status = if *ok { "ok=true" } else { "ok=false" };
                        let preview = if output.chars().count() > 200 {
                            format!("{}...", output.chars().take(200).collect::<String>())
                        } else {
                            output.clone()
                        };
                        session.append_message(
                            tiangong_core::session::MessageRole::System,
                            format!("工具执行 [{name}]\n{status}\n{preview}"),
                        );
                    }
                    StreamEvent::ToolCalls { names, usage } => {
                        // 工具调用：重置 assistant id
                        assistant_msg_id = None;
                        core_state.store.runtime.run.summary =
                            format!("正在执行：{}", names.join(", "));
                        // 累加 usage
                        if let Some(u) = usage {
                            let core_usage = tiangong_core::model::TokenUsage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                            };
                            match core_state.store.runtime.run.last_usage.as_mut() {
                                Some(existing) => existing.accumulate(&core_usage),
                                None => core_state.store.runtime.run.last_usage = Some(core_usage),
                            }
                        }
                    }
                    StreamEvent::Done { ref usage } => {
                        if let Some(u) = usage {
                            let core_usage = tiangong_core::model::TokenUsage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                            };
                            match core_state.store.runtime.run.last_usage.as_mut() {
                                Some(existing) => existing.accumulate(&core_usage),
                                None => {
                                    core_state.store.runtime.run.last_usage = Some(core_usage)
                                }
                            }
                        }
                        core_state.report_run_idle(format!(
                            "模型供应商：{}",
                            core_state.provider_label()
                        ));
                    }
                    StreamEvent::Error { message } => {
                        session.append_message(
                            tiangong_core::session::MessageRole::System,
                            format!("执行失败：{message}"),
                        );
                        core_state.report_run_idle("执行失败");
                    }
                    _ => {}
                }

                Ok(())
            });

            // emit 更新后的快照
            if let Ok(snapshot) = app_clone
                .state::<TiangongApp>()
                .with_state_read(|s| Ok(build_full_snapshot(s)))
            {
                let _ = app_clone.emit("run_snapshot", &snapshot);
            }

            if is_done || is_error {
                // 同步 TiangongCore 的完整 session 回 TiangongState 并持久化
                let final_session = core.into_session();
                let final_sid = final_session.id.clone();
                let _ = app_clone.state::<TiangongApp>().with_state(|core_state| {
                    if let Some(s) = core_state
                        .store
                        .session
                        .sessions
                        .iter_mut()
                        .find(|s| s.id == final_sid)
                    {
                        *s = final_session;
                    }
                    // 生成标题
                    if let Some(session) = core_state
                        .store
                        .session
                        .sessions
                        .iter()
                        .find(|s| s.id == final_sid)
                    {
                        let is_default = session.title == "新对话"
                            || session.title.starts_with("会话 ");
                        if is_default {
                            let user_input = session
                                .messages
                                .iter()
                                .find(|m| m.role == tiangong_core::session::MessageRole::User)
                                .map(|m| m.content.clone())
                                .unwrap_or_default();
                            if !user_input.is_empty() {
                                let client = tiangong_core::model::SingleProviderClient::new(
                                    core_state
                                        .store
                                        .provider
                                        .models_config
                                        .to_chat_provider_config(),
                                );
                                if let Ok(title) = client.complete_lite(&user_input) {
                                    let clean = title.trim().trim_matches('"').to_string();
                                    if let Some(s) = core_state
                                        .store
                                        .session
                                        .sessions
                                        .iter_mut()
                                        .find(|s| s.id == final_sid)
                                    {
                                        if !clean.is_empty() {
                                            s.title = clean.clone();
                                            s.updated_at =
                                                tiangong_core::session::now_text();
                                            core_state.store.session.session_title_draft =
                                                clean;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let _ = core_state.persist_session_and_app(&final_sid);
                    let final_snapshot = build_full_snapshot(core_state);
                    let _ = app_clone.emit("run_snapshot", &final_snapshot);
                    // 通知前端 sessions 列表已更新（标题可能变化）
                    let _ = app_clone.emit("sessions_updated", &());
                    Ok(())
                });
                break;
            }
        }
    });

    Ok(())
}

/// 取消当前执行
#[tauri::command]
pub fn cancel_turn(state: State<TiangongApp>) -> Result<bool, String> {
    state.with_state(|core_state| core_state.cancel_pending_turn())
}

/// 向正在执行的 turn 追加用户消息
#[tauri::command]
pub fn append_message(content: String, state: State<TiangongApp>) -> Result<bool, String> {
    state.with_state(|core_state| core_state.append_user_message_to_running_turn(&content))
}

/// 响应工具审批请求
#[tauri::command]
pub fn respond_approval(
    request_id: String,
    approved: bool,
    state: State<TiangongApp>,
) -> Result<bool, String> {
    state.with_state(|core_state| {
        core_state.respond_to_approval(&request_id, approved)
    })
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
    state.with_state(|core_state| {
        let trust_mode: tiangong_core::permission::TrustMode =
            serde_json::from_value(serde_json::Value::String(mode))
                .map_err(|e| anyhow::anyhow!("无效的信任模式: {e}"))?;
        core_state.set_trust_mode(trust_mode)
    })
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
                let cost = tiangong_core::observe::calculate_session_cost(&s.task_records);
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
    use tiangong_core::models_config::ModelCapability;
    use tiangong_media::tts::{SpeechSynthesizer, SynthesizeRequest};

    let resolved = state
        .with_state_read(|core_state| {
            core_state
                .models_config()
                .resolve_for_capability(ModelCapability::Tts)
                .ok_or_else(|| anyhow::anyhow!("TTS 能力未配置"))
        })
        .map_err(|e| e.to_string())?;

    let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );

    // 从模型配置 options 中读取 voice 参数
    let voice = resolved
        .options
        .get("voice")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let request = SynthesizeRequest {
        text,
        voice,
        speed: None,
        model: Some(resolved.model.clone()),
        output_format: Some("mp3".to_string()),
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        synthesizer.synthesize(request),
    )
    .await;

    match result {
        Ok(Ok(resp)) => {
            // 将音频保存到临时文件，通过 asset 协议播放
            let media_dir = user_home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".tiangong")
                .join("media");
            std::fs::create_dir_all(&media_dir)
                .map_err(|e| format!("创建媒体目录失败：{e}"))?;

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
            std::fs::write(&file_path, &resp.audio)
                .map_err(|e| format!("音频文件写入失败：{e}"))?;

            Ok(SpeechResult {
                file_path: file_path.to_string_lossy().to_string(),
                mime_type: resp.mime_type,
            })
        }
        Ok(Err(e)) => Err(format!("语音合成失败：{e}")),
        Err(_) => Err("语音合成超时（60秒）".to_string()),
    }
}

/// 检查 TTS 能力是否已配置
#[tauri::command]
pub fn has_tts_capability(state: State<TiangongApp>) -> Result<bool, String> {
    use tiangong_core::models_config::ModelCapability;
    state.with_state_read(|core_state| {
        Ok(core_state
            .models_config()
            .resolve_for_capability(ModelCapability::Tts)
            .is_some())
    })
}

/// 检查 STT 能力是否已配置
#[tauri::command]
pub fn has_stt_capability(state: State<TiangongApp>) -> Result<bool, String> {
    use tiangong_core::models_config::ModelCapability;
    state.with_state_read(|core_state| {
        Ok(core_state
            .models_config()
            .resolve_for_capability(ModelCapability::Stt)
            .is_some())
    })
}

/// 语音识别：将音频数据转录为文本，同时保存音频文件
#[tauri::command]
pub async fn transcribe_speech(
    audio_base64: String,
    mime_type: String,
    state: State<'_, TiangongApp>,
) -> Result<TranscribeResult, String> {
    use tiangong_core::models_config::ModelCapability;
    use tiangong_media::stt::{SpeechRecognizer, TranscribeRequest};

    let resolved = state
        .with_state_read(|core_state| {
            core_state
                .models_config()
                .resolve_for_capability(ModelCapability::Stt)
                .ok_or_else(|| anyhow::anyhow!("STT 能力未配置"))
        })
        .map_err(|e| e.to_string())?;

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
    std::fs::create_dir_all(&media_dir)
        .map_err(|e| format!("创建媒体目录失败：{e}"))?;

    let ext = match mime_type.as_str() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        _ => "wav",
    };
    let file_name = format!("stt_{}.{}", scru128::new(), ext);
    let file_path = media_dir.join(&file_name);
    std::fs::write(&file_path, &audio)
        .map_err(|e| format!("音频文件保存失败：{e}"))?;

    let audio_path = file_path.to_string_lossy().to_string();

    let recognizer = tiangong_media::openai_stt::OpenAIWhisper::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );

    let request = TranscribeRequest {
        audio,
        mime_type,
        language: None,
        model: Some(resolved.model.clone()),
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        recognizer.transcribe(request),
    )
    .await;

    match result {
        Ok(Ok(resp)) => Ok(TranscribeResult {
            text: resp.text,
            audio_path,
            duration: resp.duration,
        }),
        Ok(Err(e)) => Err(format!("语音识别失败：{e}")),
        Err(_) => Err("语音识别超时（120秒）".to_string()),
    }
}

/// 获取 TTS 可用音色列表
#[tauri::command]
pub async fn list_tts_voices(
    state: State<'_, TiangongApp>,
) -> Result<Vec<serde_json::Value>, String> {
    use tiangong_core::models_config::ModelCapability;
    use tiangong_media::tts::SpeechSynthesizer;

    let resolved = state
        .with_state_read(|core_state| {
            core_state
                .models_config()
                .resolve_for_capability(ModelCapability::Tts)
                .ok_or_else(|| anyhow::anyhow!("TTS 能力未配置"))
        })
        .map_err(|e| e.to_string())?;

    let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );

    let voices = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        synthesizer.list_voices(),
    )
    .await
    .map_err(|_| "获取音色列表超时".to_string())?
    .map_err(|e| format!("获取音色列表失败：{e}"))?;

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
            .args(["-c", &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", file_path)])
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
                let tool_count = active_tools.iter()
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
pub fn get_run_snapshot(state: State<TiangongApp>) -> Result<RunSnapshot, String> {
    state.with_state_read(|core_state| Ok(build_full_snapshot(core_state)))
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
pub fn get_mcp_servers(state: State<TiangongApp>) -> Result<Vec<McpServer>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .mcp_servers()
            .iter()
            .map(McpServer::from_core)
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

    state.with_state(|core_state| {
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
    })
}

/// 移除 MCP 服务器
#[tauri::command]
pub fn remove_mcp_server(name: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.remove_mcp_server(&name))
}

/// 设置 MCP 服务器启用状态
#[tauri::command]
pub fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    state: State<TiangongApp>,
) -> Result<String, String> {
    state.with_state(|core_state| core_state.set_mcp_server_enabled(&name, enabled))
}

// ============================================================================
// Skill 管理
// ============================================================================

/// 获取已安装的 Skill 列表
#[tauri::command]
pub fn get_skills(state: State<TiangongApp>) -> Result<Vec<Skill>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .installed_skills()
            .iter()
            .map(Skill::from_core)
            .collect())
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
    state.with_state(|core_state| {
        let env: Vec<(String, String)> = env_values
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .collect();
        core_state.install_local_skill_with_options_and_inputs(&path, true, true, &env)
    })
}

/// 移除 Skill
#[tauri::command]
pub fn remove_skill(id: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.remove_skill(&id))
}

/// 获取 Skill 的环境变量（合并 skill.toml 声明的 requires.env + .env.local 已有值）
#[tauri::command]
pub fn get_skill_env(id: String, state: State<TiangongApp>) -> Result<std::collections::HashMap<String, String>, String> {
    state.with_state_read(|core_state| {
        let skill = core_state.installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;

        let skill_dir = std::path::Path::new(&skill.source.value);
        let mut env = std::collections::HashMap::new();

        // 1. 从 skill.toml 的 requires.env 读取声明的 key（值为空）
        let toml_path = skill_dir.join("skill.toml");
        if let Ok(raw) = std::fs::read_to_string(&toml_path) {
            #[derive(serde::Deserialize, Default)]
            struct T { #[serde(default)] requires: R }
            #[derive(serde::Deserialize, Default)]
            struct R { #[serde(default)] env: Vec<String> }
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
                if line.is_empty() || line.starts_with('#') { continue; }
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
        let skill = core_state.installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;
        let env_path = std::path::Path::new(&skill.source.value).join(".env.local");
        let lines: Vec<String> = env.iter()
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
    state.with_state(|core_state| core_state.set_skill_enabled(&id, enabled))
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
    if let Some(profile) =
        std::env::var_os("USERPROFILE").filter(|v| v != std::ffi::OsStr::new(""))
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
pub fn get_connectors() -> Result<Vec<ConnectorInfo>, String> {
    let configs = tiangong_server::config::load_connectors_config();
    Ok(configs.iter().map(ConnectorInfo::from_config).collect())
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
    state.with_state_read(|core_state| {
        Ok(ModelsConfigView::from_core(core_state.models_config()))
    })
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
    })
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
) -> Result<Vec<String>, String> {
    use tiangong_core::model::{ModelProviderConfig, SingleProviderClient};
    use tiangong_core::models_config::ModelsConfig;

    let resolved_key = ModelsConfig::resolve_api_key(&api_key);
    let config = ModelProviderConfig {
        api_auth_token: resolved_key,
        api_base_url: base_url,
        api_timeout_ms: timeout_ms.unwrap_or(60_000).to_string(),
        api_model: String::new(),
        api_lite_model: String::new(),
    };
    SingleProviderClient::list_models(&config).map_err(|e| e.to_string())
}
