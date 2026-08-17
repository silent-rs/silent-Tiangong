//! System Prompt Sections
//!
//! core 只负责 Prompt 段落的顺序组装、会话运行上下文与摘要合并。
//! 产品身份、回复风格、多媒体能力说明等文案由各插件经 `PromptSectionProvider`
//! 注入（产品基础文案见 `tiangong-plugin-prompt`，能力说明见对应能力插件）。

use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};

/// 构建 system prompt 所需的动态配置数据快照
///
/// 由调用方从插件收集段落，传入 `session.rebuild_system_prompt()`。
pub struct SystemPromptConfig {
    /// Plugin 注入的段落（产品身份 / 通用规则 / 自定义指令外围 / 各能力插件说明等）。
    /// 按插件注册顺序追加；产品文案插件注册在最前，保证身份与规则排在 prompt 开头。
    pub plugin_sections: Vec<String>,
}

impl SystemPromptConfig {
    /// 注入 Plugin 提供的 prompt 段落
    pub fn from_plugin_sections(sections: Vec<String>) -> Self {
        Self {
            plugin_sections: sections,
        }
    }
}

/// 构建完整的 system prompt 消息
///
/// 组装顺序：插件段（含产品身份 / 通用规则 / 自定义指令 / 各能力说明）
/// → 环境段（会话标题 / 工作目录 / 文件根）→ 摘要段。
/// 返回 `Message { role: System }`，由 `build_provider_messages()` 提取到 system prompt。
pub fn build_full_system_prompt(session: &Session, config: &SystemPromptConfig) -> Message {
    let mut parts = Vec::new();

    // 插件段（产品文案 + 各能力插件段落，按注册顺序）
    for section in &config.plugin_sections {
        let trimmed = section.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    // 环境段
    parts.extend(collect_environment_parts(session));

    // 摘要段
    parts.extend(collect_summary_part(session));

    assemble_system_message(parts)
}

/// 收集环境段（会话标题、工作目录、文件根）
fn collect_environment_parts(session: &Session) -> Vec<String> {
    let mut parts = Vec::new();
    let workspace = session_working_directory(session);
    parts.push(format!("当前会话：{}", session.title));
    parts.push(format!("当前工作目录：{}", workspace));
    // 允许文件操作目录：工作空间 + 应用存储根（由 toolkit 硬编码为始终允许）。
    parts.push(format!(
        "允许文件操作目录：{}；{}",
        workspace,
        tiangong_toolkit::app_storage_root().display()
    ));
    parts
}

fn session_working_directory(session: &Session) -> String {
    let cwd = session.cwd.trim();
    if !cwd.is_empty() {
        return std::path::PathBuf::from(cwd)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(cwd))
            .display()
            .to_string();
    }

    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into())
}

/// 收集摘要段
fn collect_summary_part(session: &Session) -> Vec<String> {
    if let Some(summary) = session
        .context_summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        vec![format!("此前对话摘要：\n{summary}")]
    } else {
        Vec::new()
    }
}

/// 组装最终的 System Message
fn assemble_system_message(parts: Vec<String>) -> Message {
    Message {
        id: scru128::new().to_string(),
        role: MessageRole::System,
        content: vec![crate::session::ContentBlock::text(parts.join("\n\n"))],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        elapsed_ms: None,
        turn_status: None,
        reasoning_elapsed_ms: None,
        text_elapsed_ms: None,
        duration_ms: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_full_system_prompt_with_empty_sections_still_valid() {
        // 未注入任何插件段落时，core 仍应构建仅含环境段的合法 system prompt。
        let session = Session::new("测试会话");
        let config = SystemPromptConfig::from_plugin_sections(Vec::new());
        let msg = build_full_system_prompt(&session, &config);
        assert_eq!(msg.role, MessageRole::System);
        let text = msg.content.first().unwrap().as_text().unwrap_or_default();
        assert!(text.contains("当前会话：测试会话"));
        assert!(text.contains("当前工作目录"));
    }

    #[test]
    fn build_full_system_prompt_includes_plugin_sections_in_order() {
        let session = Session::new("测试");
        let config = SystemPromptConfig::from_plugin_sections(vec![
            "身份段".to_string(),
            "规则段".to_string(),
            "  ".to_string(), // 空白段应被过滤
        ]);
        let msg = build_full_system_prompt(&session, &config);
        let text = msg.content.first().unwrap().as_text().unwrap_or_default();
        let id_idx = text.find("身份段").unwrap();
        let rule_idx = text.find("规则段").unwrap();
        assert!(id_idx < rule_idx);
    }
}
