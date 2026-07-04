//! ReAct 循环上下文管理：system prompt 重建、token usage 上报、上下文压缩。
//!
//! 最终化相关逻辑（总结阶段、强制最终回复）已迁移到 `summary.rs`。

use std::sync::mpsc::Sender as StdSender;

use crate::context::organizer::ContextOrganizer;
use crate::model::{ModelRequest, SingleProviderClient, TokenUsage};
use crate::prompt::SystemPromptConfig;
use crate::runtime::RuntimeEngine;
use crate::session::Session;
use tiangong_types::StreamEvent;

/// 从 RuntimeEngine 配置构建 SystemPromptConfig 并重建 session 的 system prompt
pub(crate) fn rebuild_system_prompt(session: &mut Session, engine: &RuntimeEngine) {
    let plugin_sections = engine.collect_plugin_prompt_sections();
    let config = SystemPromptConfig::from_configs(engine.models_config(), engine.agent_config())
        .with_plugin_sections(plugin_sections);
    session.rebuild_system_prompt(&config);
}

pub(crate) fn compression_threshold_tokens(context_limit: usize) -> usize {
    ContextOrganizer::new(context_limit)
        .with_threshold(0.95)
        .token_threshold()
}

pub(crate) fn emit_token_usage(
    stream_tx: &StdSender<StreamEvent>,
    usage: &TokenUsage,
    current_tokens: Option<usize>,
    context_limit: usize,
    source: impl Into<String>,
    agent_id: Option<&str>,
) {
    if usage.total_tokens == 0 {
        return;
    }
    let source = source.into();
    // 按 source（阶段）记录 KV cache 命中率，便于实测 ReAct/Summary 各阶段命中情况。
    // hit_ratio = hit / (hit + miss)，仅当 DeepSeek 返回了 cache 字段时记录。
    if let (Some(hit), Some(miss)) = (
        usage.prompt_cache_hit_tokens,
        usage.prompt_cache_miss_tokens,
    ) {
        let total = hit + miss;
        let ratio = if total > 0 {
            hit as f64 / total as f64
        } else {
            0.0
        };
        tracing::info!(
            source = %source,
            agent_id = ?agent_id,
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            prompt_cache_hit_tokens = hit,
            prompt_cache_miss_tokens = miss,
            cache_hit_ratio = format!("{:.2}%", ratio * 100.0),
            "kv cache 命中统计",
        );
    }
    let _ = stream_tx.send(StreamEvent::TokenUsage {
        usage: usage.clone(),
        current_tokens,
        compression_threshold_tokens: Some(compression_threshold_tokens(context_limit)),
        context_limit_tokens: Some(context_limit),
        source,
        agent_id: agent_id.map(|s| s.to_string()),
    });
}

/// 处理插件上报的 token 用量：累加到本轮 `accumulated_usage` 并统一发送
/// `StreamEvent::TokenUsage`。
///
/// 用于 react engine 中处理 [`crate::core::command::Command::ReportPluginUsage`]：
/// 插件（如 `analyze_attachment`）在工具执行中产生的 LLM 子调用用量经此入口计入
/// 本轮统计，确保最终 `Done.usage`、成本统计与上下文压缩判断都包含该消耗。
pub(crate) fn handle_plugin_usage(
    accumulated_usage: &mut TokenUsage,
    stream_tx: &StdSender<StreamEvent>,
    context_limit: usize,
    usage: &TokenUsage,
    source: &str,
    agent_id: Option<&str>,
) {
    accumulated_usage.accumulate(usage);
    emit_token_usage(stream_tx, usage, None, context_limit, source, agent_id);
}

pub(crate) fn maybe_update_context_summary(
    session: &mut Session,
    engine: &RuntimeEngine,
    observed_usage: &TokenUsage,
    stream_tx: &StdSender<StreamEvent>,
) {
    let organizer = ContextOrganizer::new(engine.context_limit)
        .with_threshold(0.95)
        .with_keep_recent_turns(6);
    let observed_tokens = observed_total_tokens(observed_usage);
    if !organizer.needs_compression(observed_tokens) {
        return;
    }
    let total_messages = session.messages.len();
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages,
    });
    match organizer.maybe_update_summary_with_usage(session, engine.client(), observed_tokens) {
        Ok(update) if update.compressed => {
            // 重置 current_tokens，压缩后上下文已大幅缩减
            session.current_tokens = 0;
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            // 估算压缩后的 token 数（基于剩余消息比例）
            let estimated_tokens = (observed_tokens as f64
                * (remaining as f64 / total_messages.max(1) as f64))
                as usize;
            session.current_tokens = estimated_tokens;
            // 压缩后重建 system prompt（摘要已更新）
            rebuild_system_prompt(session, engine);
            emit_token_usage(
                stream_tx,
                &update.usage,
                Some(estimated_tokens),
                engine.context_limit,
                "context_summary",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Auto,
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
            tracing::info!(
                session_id = %session.id,
                observed_tokens,
                observed_prompt_tokens = observed_usage.prompt_tokens,
                observed_completion_tokens = observed_usage.completion_tokens,
                threshold_tokens = organizer.token_threshold(),
                summary_up_to = session.summary_up_to,
                "上下文与本轮输出达到压缩阈值，已更新早期对话摘要"
            );
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                session_id = %session.id,
                error = %err,
                "上下文压缩失败，继续使用原始上下文"
            );
        }
    }
}

pub(crate) fn observed_total_tokens(usage: &TokenUsage) -> usize {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.prompt_tokens.saturating_add(usage.completion_tokens)
    }
}

pub(crate) fn select_client_for_request<'a>(
    engine: &'a RuntimeEngine,
    _req: &ModelRequest,
) -> &'a SingleProviderClient {
    engine.client()
}

/// 将错误持久化到 session，作为 LLM 请求失败时的诊断痕迹。
///
/// 复用 `inject_tool_to_messages` 统一注入通道（合法的 assistant tool_call + tool result
/// 消息对），而非手工追加消息：
/// - 避免使用 `MessageRole::System`——它会被 `build_provider_messages` 的
///   `system_texts.clear()` 覆盖整个 system prompt，污染 prompt 并破坏 KV cache 前缀；
/// - 走标准注入通道保证 append-only、provider 序列化为合法 tool pair，cache 友好。
///
/// 去重边界：`inject_tool_to_messages` 仅对连续相同的 plugin_injection 结果去重，
/// 不会与前端 `StreamEvent::Error` 落盘的 `[错误]` System 消息跨格式去重——因此
/// 前端若也落盘，UI 上仍可能出现重复错误消息。engine 侧落盘作为前端时序丢失的
/// 兜底，确保会话重载后至少能看到失败原因。
pub(crate) fn persist_error(session: &mut Session, message: impl Into<String>) {
    let message = message.into();
    let payload = serde_json::json!({
        "error": message,
        "instruction": "上一步执行失败，请基于已有结果继续或向用户说明原因。",
    });
    crate::react::message::inject_tool_to_messages(session, "react_loop_error", &payload);
    session.persist_to_disk();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_total_tokens_prefers_provider_total() {
        let usage = TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 120,
            total_tokens: 1100,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        assert_eq!(observed_total_tokens(&usage), 1100);
    }

    #[test]
    fn observed_total_tokens_falls_back_to_prompt_plus_completion() {
        let usage = TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 120,
            total_tokens: 0,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        assert_eq!(observed_total_tokens(&usage), 1020);
    }
}
