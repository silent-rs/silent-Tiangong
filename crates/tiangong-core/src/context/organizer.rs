use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use crate::context::compressor::{CompressionUpdate, ContextCompressor};
use crate::model::SingleProviderClient;
use crate::session::{Message, MessageRole, Session, now_text};

const KEEP_RECENT_EXECUTION_ROUNDS: usize = 4;
const EXECUTION_ROUND_COMPACT_THRESHOLD: usize = 6;

/// 上下文组织器
///
/// 管理对话上下文的构建与压缩策略。
/// 采用滚动摘要机制：摘要持久化到 Session，原始消息保持完整。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.95，接近模型限制前压缩）
    compression_threshold: f64,
    /// 压缩器
    compressor: ContextCompressor,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.95,
            compressor: ContextCompressor::default(),
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compression_threshold = threshold;
        self
    }

    pub fn with_max_context_tokens(mut self, max_tokens: usize) -> Self {
        if self.context_limit > 0 {
            self.compression_threshold =
                (max_tokens as f64 / self.context_limit as f64).clamp(0.0, 1.0);
        }
        self
    }

    pub fn with_keep_recent_turns(mut self, turns: usize) -> Self {
        self.compressor = ContextCompressor::new(turns);
        self
    }

    /// 压缩阈值（token 数）
    pub fn token_threshold(&self) -> usize {
        (self.context_limit as f64 * self.compression_threshold) as usize
    }

    /// 基于 API 返回的精确 prompt_tokens 判断是否需要压缩
    pub fn needs_compression(&self, actual_prompt_tokens: usize) -> bool {
        actual_prompt_tokens > self.token_threshold()
    }

    /// 基于 API 返回的精确 prompt_tokens 更新会话摘要（如果需要）
    ///
    /// 检查实际 prompt token 是否超过阈值，如果超过则更新 session 的滚动摘要。
    /// 摘要持久化到 session，后续 turn 不会重复压缩。
    pub fn maybe_update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        actual_prompt_tokens: usize,
    ) -> anyhow::Result<bool> {
        Ok(self
            .maybe_update_summary_with_usage(session, client, actual_prompt_tokens)?
            .compressed)
    }

    pub fn maybe_update_summary_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        actual_prompt_tokens: usize,
    ) -> anyhow::Result<CompressionUpdate> {
        if actual_prompt_tokens == 0 || !self.needs_compression(actual_prompt_tokens) {
            return Ok(CompressionUpdate::default());
        }
        self.compressor.update_summary_with_usage(session, client)
    }

    /// 强制压缩上下文（忽略 token 阈值检查）
    pub fn force_update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> anyhow::Result<bool> {
        Ok(self
            .force_update_summary_with_usage(session, client)?
            .compressed)
    }

    pub fn force_update_summary_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> anyhow::Result<CompressionUpdate> {
        self.compressor.update_summary_with_usage(session, client)
    }

    /// 构建 LLM 请求上下文
    ///
    /// 从 session 的摘要 + 最近消息构建，并过滤执行痕迹。
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        let raw = self.compressor.build_context(session);
        let filtered = Self::filter_execution_traces_vec(&raw);
        Self::compact_execution_rounds_for_prompt(&filtered)
    }

    /// 过滤执行阶段的内部 Tool 消息。
    ///
    /// `LLM 输出` 等内部痕迹仍会被丢弃；`工具执行` 会被压缩为摘要保留，
    /// 避免跨用户轮次丢失“刚刚读过/定位过的文件路径”等关键现场信息。
    pub fn filter_execution_traces(messages: &[Message]) -> Vec<Message> {
        Self::filter_execution_traces_vec(messages)
    }

    fn filter_execution_traces_vec(messages: &[Message]) -> Vec<Message> {
        let mut seen_tool_result_keys = HashSet::new();
        let mut filtered = messages
            .iter()
            .filter_map(|msg| {
                if msg.role != MessageRole::Tool {
                    return Some(msg.clone());
                }
                let c = msg.content.as_str();
                // 保留摘要消息
                if c.starts_with("[早期对话摘要]") {
                    return Some(msg.clone());
                }
                if c.starts_with("工具执行") {
                    let mut summarized = msg.clone();
                    summarized.content = summarize_tool_execution_trace(c);
                    return Some(summarized);
                }
                if c.starts_with("LLM 输出")
                    || c.starts_with("Plan 执行总结")
                    || c.starts_with("检测到")
                    || c.starts_with("执行已取消")
                {
                    return None;
                }
                if msg.tool_call_id.is_some() {
                    let mut summarized = msg.clone();
                    let key = historical_tool_result_key(msg);
                    if !key.is_empty() && !seen_tool_result_keys.insert(key) {
                        summarized.content = duplicate_tool_result_summary(msg);
                    } else if c.chars().count() > 5000 {
                        summarized.content = summarize_historical_tool_result(msg);
                    }
                    return Some(summarized);
                }
                Some(msg.clone())
            })
            .collect::<Vec<_>>();

        Self::strip_stale_reasoning_for_prompt(&mut filtered);
        filtered
    }

    fn strip_stale_reasoning_for_prompt(messages: &mut [Message]) {
        let keep_reasoning_index = messages.iter().rposition(|msg| {
            msg.role == MessageRole::Assistant
                && !msg.tool_calls.is_empty()
                && !msg.reasoning_content.trim().is_empty()
        });

        for (index, msg) in messages.iter_mut().enumerate() {
            if msg.role != MessageRole::Assistant || msg.reasoning_content.trim().is_empty() {
                continue;
            }
            if Some(index) == keep_reasoning_index {
                continue;
            }
            msg.reasoning_content.clear();
            msg.reasoning_signature = None;
        }
    }

    fn compact_execution_rounds_for_prompt(messages: &[Message]) -> Vec<Message> {
        let mut result = Vec::with_capacity(messages.len());
        let mut index = 0;

        while index < messages.len() {
            if messages[index].role != MessageRole::User {
                result.push(messages[index].clone());
                index += 1;
                continue;
            }

            result.push(messages[index].clone());
            index += 1;

            let segment_start = index;
            while index < messages.len() && messages[index].role != MessageRole::User {
                index += 1;
            }
            let segment = &messages[segment_start..index];
            result.extend(compact_execution_segment(segment));
        }

        result
    }
}

fn compact_execution_segment(segment: &[Message]) -> Vec<Message> {
    if segment.is_empty() {
        return Vec::new();
    }

    let rounds = execution_rounds(segment);
    if rounds.len() <= EXECUTION_ROUND_COMPACT_THRESHOLD {
        return segment.to_vec();
    }

    let compact_round_count = rounds.len().saturating_sub(KEEP_RECENT_EXECUTION_ROUNDS);
    if compact_round_count == 0 {
        return segment.to_vec();
    }

    let compact_end = rounds[compact_round_count - 1].1;
    let early_messages = &segment[..compact_end];
    let recent_messages = &segment[compact_end..];

    let mut compacted = vec![Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content: summarize_execution_rounds(early_messages, compact_round_count),
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("react_context_compaction".to_string()),
        tool_result_is_error: false,
        compact: true,
        created_at: now_text(),
    }];
    compacted.extend_from_slice(recent_messages);
    compacted
}

fn execution_rounds(messages: &[Message]) -> Vec<(usize, usize)> {
    let mut rounds = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let start = index;
        index += 1;

        if messages[start].role == MessageRole::Assistant {
            while index < messages.len() && messages[index].role == MessageRole::Tool {
                index += 1;
            }
        }

        rounds.push((start, index));
    }
    rounds
}

fn summarize_execution_rounds(messages: &[Message], round_count: usize) -> String {
    let mut lines = vec![format!(
        "[早期执行过程摘要]\n已压缩 {round_count} 轮早期执行过程，仅保留关键工具调用和结果线索。"
    )];

    for msg in messages {
        match msg.role {
            MessageRole::Assistant => {
                let tools = msg
                    .tool_calls
                    .iter()
                    .map(|call| call.name.as_str())
                    .collect::<Vec<_>>();
                if !tools.is_empty() {
                    lines.push(format!("助手调用工具: {}", tools.join(", ")));
                } else if !msg.content.trim().is_empty() {
                    lines.push(format!("助手回复: {}", truncate_chars(&msg.content, 240)));
                }
            }
            MessageRole::Tool => {
                let tool_name = msg.tool_name.as_deref().unwrap_or("tool");
                let status = if msg.tool_result_is_error {
                    "失败"
                } else {
                    "成功"
                };
                lines.push(format!(
                    "工具 {tool_name} {status}: {}",
                    truncate_chars(&msg.content, 360)
                ));
            }
            MessageRole::System => {
                lines.push(format!("系统上下文: {}", truncate_chars(&msg.content, 240)));
            }
            MessageRole::User => {
                lines.push(format!("用户补充: {}", truncate_chars(&msg.content, 240)));
            }
        }

        if lines.len() >= 24 {
            lines.push("...(更多早期执行细节已省略)".to_string());
            break;
        }
    }

    lines.join("\n")
}

fn summarize_tool_execution_trace(content: &str) -> String {
    let mut kept_lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("stdout:") || trimmed.starts_with("stderr:") {
            kept_lines.push(format!("{trimmed} ...(已省略原始输出)"));
            break;
        }
        if trimmed.starts_with("```") {
            break;
        }
        kept_lines.push(line.to_string());
        if kept_lines.len() >= 12 {
            kept_lines.push("...(已截断工具执行摘要)".to_string());
            break;
        }
    }

    format!("[工具执行摘要]\n{}", kept_lines.join("\n"))
}

fn summarize_historical_tool_result(msg: &Message) -> String {
    let tool_name = msg.tool_name.as_deref().unwrap_or("unknown");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        let mut lines = vec![format!("[历史工具结果摘要]\ntool: {tool_name}")];
        for key in [
            "mode",
            "url",
            "final_url",
            "status",
            "content_type",
            "title",
            "truncated",
            "bytes_read",
            "saved_path",
            "path",
        ] {
            if let Some(item) = value.get(key) {
                lines.push(format!("{key}: {}", compact_json_value(item, 300)));
            }
        }
        if let Some(text) = value.get("summary").and_then(serde_json::Value::as_str) {
            lines.push(format!("summary: {}", truncate_chars(text, 1200)));
        } else if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
            lines.push(format!("text_preview: {}", truncate_chars(text, 2200)));
        } else if let Some(stdout) = value.get("stdout").and_then(serde_json::Value::as_str) {
            lines.push(format!("stdout_preview: {}", truncate_chars(stdout, 2200)));
        }
        return lines.join("\n");
    }

    format!(
        "[历史工具结果摘要]\ntool: {tool_name}\ncontent_preview: {}",
        truncate_chars(&msg.content, 2600)
    )
}

fn duplicate_tool_result_summary(msg: &Message) -> String {
    let tool_name = msg.tool_name.as_deref().unwrap_or("unknown");
    let key = historical_tool_result_display_key(msg);
    if key.is_empty() {
        format!("[重复工具结果已省略]\ntool: {tool_name}")
    } else {
        format!("[重复工具结果已省略]\ntool: {tool_name}\nkey: {key}")
    }
}

fn historical_tool_result_key(msg: &Message) -> String {
    let tool_name = msg.tool_name.as_deref().unwrap_or("unknown");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        let mode = value
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        for key in ["url", "final_url", "path", "saved_path"] {
            if let Some(item) = value.get(key).and_then(serde_json::Value::as_str) {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    return format!("{tool_name}:mode:{mode}:{key}:{trimmed}");
                }
            }
        }
    }

    let normalized = msg
        .content
        .split_whitespace()
        .take(256)
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return String::new();
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    normalized.hash(&mut hasher);
    format!("{tool_name}:hash:{:x}", hasher.finish())
}

fn historical_tool_result_display_key(msg: &Message) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        for key in ["url", "final_url", "path", "saved_path"] {
            if let Some(item) = value.get(key).and_then(serde_json::Value::as_str) {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    return truncate_chars(trimmed, 240);
                }
            }
        }
    }
    String::new()
}

fn compact_json_value(value: &serde_json::Value, max_chars: usize) -> String {
    match value {
        serde_json::Value::String(text) => truncate_chars(text, max_chars),
        other => truncate_chars(&other.to_string(), max_chars),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated = normalized.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::now_text;
    use serde_json::json;

    fn message(role: MessageRole, content: &str) -> Message {
        Message {
            id: scru128::new().to_string(),
            role,
            content: content.to_string(),
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }

    fn tool_result(tool_call_id: &str, tool_name: &str, content: String) -> Message {
        Message {
            id: scru128::new().to_string(),
            role: MessageRole::Tool,
            content,
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }

    fn assistant_with_tool_call(id: &str, tool_name: &str, reasoning: &str) -> Message {
        Message {
            id: scru128::new().to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            reasoning_content: reasoning.to_string(),
            reasoning_signature: Some(format!("sig-{id}")),
            worker_id: None,
            media: Vec::new(),
            tool_calls: vec![crate::session::MessageToolCall {
                id: id.to_string(),
                name: tool_name.to_string(),
                arguments: serde_json::json!({}),
            }],
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }

    #[test]
    fn filter_execution_traces_keeps_tool_execution_summary_paths() {
        let messages = vec![
            message(MessageRole::User, "直接帮我修复这个脚本就行了"),
            message(MessageRole::Tool, "LLM 输出\ntool_calls: read_file"),
            message(
                MessageRole::Tool,
                "工具执行 [read_file]\n命令: /Users/example/.tiangong/skills/installed/web-search/1.0.0/web_search.py\nok=true exit_code=0\nsummary: read_file\nstdout:\n     1\t#!/usr/bin/env python3\n     2\tfrom duckduckgo_search import DDGS",
            ),
        ];

        let filtered = ContextOrganizer::filter_execution_traces(&messages);

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].role, MessageRole::User);
        assert!(filtered[1].content.starts_with("[工具执行摘要]"));
        assert!(
            filtered[1].content.contains(
                "/Users/example/.tiangong/skills/installed/web-search/1.0.0/web_search.py"
            )
        );
        assert!(filtered[1].content.contains("stdout: ...(已省略原始输出)"));
        assert!(!filtered[1].content.contains("from duckduckgo_search"));
    }

    #[test]
    fn filter_execution_traces_summarizes_large_historical_tool_results() {
        let content = json!({
            "url": "https://example.com/large",
            "status": 200,
            "title": "Large Page",
            "text": "正文".repeat(4000),
            "truncated": true,
            "bytes_read": 120000
        })
        .to_string();

        let filtered = ContextOrganizer::filter_execution_traces(&[tool_result(
            "call_1",
            "web_fetch",
            content,
        )]);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].content.starts_with("[历史工具结果摘要]"));
        assert!(filtered[0].content.contains("https://example.com/large"));
        assert!(filtered[0].content.chars().count() < 5000);
    }

    #[test]
    fn filter_execution_traces_deduplicates_repeated_historical_tool_results() {
        let content = json!({
            "url": "https://example.com/repeated",
            "status": 200,
            "text": "same"
        })
        .to_string();

        let filtered = ContextOrganizer::filter_execution_traces(&[
            tool_result("call_1", "web_fetch", content.clone()),
            tool_result("call_2", "web_fetch", content),
        ]);

        assert_eq!(filtered.len(), 2);
        assert!(filtered[1].content.starts_with("[重复工具结果已省略]"));
        assert!(filtered[1].content.contains("https://example.com/repeated"));
    }

    #[test]
    fn build_context_strips_stale_reasoning_and_compacts_old_rounds() {
        let mut session = Session::new("上下文压缩验证");
        session.append_message(MessageRole::User, "请连续读取多个文件".to_string());
        for index in 0..8 {
            let call_id = format!("call_{index}");
            session.messages.push(assistant_with_tool_call(
                &call_id,
                "read_file",
                &"思考".repeat(300),
            ));
            session.messages.push(tool_result(
                &call_id,
                "read_file",
                format!("文件内容 {}", "正文".repeat(800)),
            ));
        }

        let organizer = ContextOrganizer::new(32768);
        let context = organizer.build_context(&session);

        assert!(
            context
                .iter()
                .any(|msg| msg.content.starts_with("[早期执行过程摘要]"))
        );
        let reasoning_count = context
            .iter()
            .filter(|msg| !msg.reasoning_content.is_empty())
            .count();
        assert_eq!(reasoning_count, 1);
    }
}
