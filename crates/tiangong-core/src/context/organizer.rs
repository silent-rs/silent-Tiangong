use crate::context::compressor::ContextCompressor;
use crate::model::SingleProviderClient;
use crate::session::{Message, MessageRole, Session};

/// token 估算（仅用于首次调用前的预判，后续应使用 API 返回的精确值）
///
/// 中文约 1.5 token/字符，英文约 0.75 token/word，
/// 使用 字符数 * 0.6 作为混合场景近似值。
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content_chars = msg.content.chars().count();
            let reasoning_chars = msg.reasoning_content.chars().count();
            // 每条消息额外 4 token 开销（role/separator）
            ((content_chars + reasoning_chars) as f64 * 0.6) as usize + 4
        })
        .sum()
}

/// 上下文组织器
///
/// 管理对话上下文的构建与压缩策略。
/// 采用滚动摘要机制：摘要持久化到 Session，原始消息保持完整。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.7，留 30% 给当前请求和回复）
    compression_threshold: f64,
    /// 压缩器
    compressor: ContextCompressor,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.7,
            compressor: ContextCompressor::default(),
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compression_threshold = threshold;
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

    /// 基于估算判断是否需要压缩（首次调用前使用）
    pub fn needs_compression_estimated(&self, session: &Session) -> bool {
        // 构建当前会发送给 LLM 的上下文，估算其 token 量
        let context = self.compressor.build_context(session);
        let filtered = Self::filter_execution_traces_vec(&context);
        estimate_tokens(&filtered) > self.token_threshold()
    }

    /// 基于 API 返回的精确 prompt_tokens 判断是否需要压缩
    pub fn needs_compression(&self, actual_prompt_tokens: usize) -> bool {
        actual_prompt_tokens > self.token_threshold()
    }

    /// 在 turn 开始前更新会话摘要（如果需要）
    ///
    /// 检查当前上下文是否超过阈值，如果超过则更新 session 的滚动摘要。
    /// 摘要持久化到 session，后续 turn 不会重复压缩。
    pub fn maybe_update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> anyhow::Result<bool> {
        if !self.needs_compression_estimated(session) {
            return Ok(false);
        }
        self.compressor.update_summary(session, client)
    }

    /// 构建 LLM 请求上下文
    ///
    /// 从 session 的摘要 + 最近消息构建，并过滤执行痕迹。
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        let raw = self.compressor.build_context(session);
        Self::filter_execution_traces_vec(&raw)
    }

    /// 过滤执行阶段的 System 消息。
    ///
    /// `LLM 输出` 等内部痕迹仍会被丢弃；`工具执行` 会被压缩为摘要保留，
    /// 避免跨用户轮次丢失“刚刚读过/定位过的文件路径”等关键现场信息。
    pub fn filter_execution_traces(messages: &[Message]) -> Vec<Message> {
        Self::filter_execution_traces_vec(messages)
    }

    fn filter_execution_traces_vec(messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .filter_map(|msg| {
                if msg.role != MessageRole::System {
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
                Some(msg.clone())
            })
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::now_text;

    fn message(role: MessageRole, content: &str) -> Message {
        Message {
            id: scru128::new().to_string(),
            role,
            content: content.to_string(),
            reasoning_content: String::new(),
            worker_id: None,
            media: Vec::new(),
            created_at: now_text(),
        }
    }

    #[test]
    fn filter_execution_traces_keeps_tool_execution_summary_paths() {
        let messages = vec![
            message(MessageRole::User, "直接帮我修复这个脚本就行了"),
            message(MessageRole::System, "LLM 输出\ntool_calls: read_file"),
            message(
                MessageRole::System,
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
}
