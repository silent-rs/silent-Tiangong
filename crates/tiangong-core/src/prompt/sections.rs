//! System Prompt Sections
//!
//! 静态块和动态块的具体内容。
//! 静态块跨会话稳定，动态块按会话/配置变化。

use crate::agent_config::AgentConfig;
use crate::models_config::ModelsConfig;
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};

/// 身份块
fn identity_block() -> String {
    "你是天工智能助手，一个功能丰富的个人 AI 中枢。你可以回答问题、处理文件、执行命令、生成多媒体内容，也可以通过工具和扩展能力完成各种复杂任务。".to_string()
}

/// 规则块
fn rules_block() -> String {
    "规则：
1. 对话时自然友好，回复内容完整有用。闲聊和问候时正常交流，简单介绍自己的能力。
2. 需要文件操作、代码搜索、命令执行等实际操作时，调用对应的工具。
3. 每次工具调用后会收到执行结果，根据结果决定下一步：继续调用工具或给出最终回复。
4. 执行工具任务时语言简洁高效，不要说\"让我查看\"之类的过渡语，直接给出结果。
5. 不要在回复中包含工具调用的原始痕迹（如 ok=、exit_code= 等元数据）。
6. 回复使用 Markdown 格式：代码和命令用代码块包裹，使用标题、列表等结构化排版。
7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。
8. 命令执行默认使用 run_shell 或 run_command，并根据工具结果继续推进。
9. 只有用户明确要求后台、不阻塞、并行、持续运行、启动服务/监听，或需要管理已有后台任务时，才使用 spawn_task / wait_tasks。"
        .to_string()
}

/// 多媒体能力 section
fn build_media_section(models_config: &ModelsConfig) -> String {
    use crate::models_config::{ModelCapability, RoutingSlot};

    let mut hints = Vec::new();
    for cap in ModelCapability::media_capabilities() {
        let slot = RoutingSlot::from_capability(*cap);
        if let Some(resolved) = models_config.resolve_slot(slot) {
            hints.push(format!(
                "- {}：已配置（模型：{}）",
                cap.display_name(),
                resolved.model
            ));
        }
    }
    if hints.is_empty() {
        return String::new();
    }
    format!("已配置的多媒体能力：\n{}", hints.join("\n"))
}

/// 构建 System Context（环境事实，追加到 system prompt 尾部）
pub fn build_system_context(session: &Session) -> Vec<String> {
    let mut ctx = Vec::new();

    let workspace = session_working_directory(session);
    ctx.push(format!("当前工作目录：{}", workspace));
    // 允许文件操作目录：仅工作空间。插件贡献的额外根目录
    // 由各插件经 prompt section 自行注入，core 不再硬编码。
    ctx.push(format!("允许文件操作目录：{}", workspace));

    ctx
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

/// 构建 system prompt 所需的动态配置数据快照
///
/// 由调用方从 AgentConfig / ModelsConfig 构建，传入 session.rebuild_system_prompt()。
pub struct SystemPromptConfig {
    pub custom_prompt: String,
    pub media_text: String,
    /// Plugin 注入的额外段落（如终端交互引导、浏览器使用规范、Skill 摘要、团队协作等）
    pub plugin_sections: Vec<String>,
}

impl SystemPromptConfig {
    /// 从配置实例构建动态数据快照
    pub fn from_configs(models_config: &ModelsConfig, agent_config: &AgentConfig) -> Self {
        Self {
            custom_prompt: agent_config.custom_system_prompt.trim().to_string(),
            media_text: build_media_section(models_config),
            // 团队协作指引已迁移至 team 插件（tiangong-plugin-agent-team）的
            // PromptSectionProvider，不再由 core 硬编码。
            plugin_sections: Vec::new(),
        }
    }

    /// 注入 Plugin 提供的 prompt 段落（在 from_configs 基础上追加）
    pub fn with_plugin_sections(mut self, sections: Vec<String>) -> Self {
        self.plugin_sections = sections;
        self
    }
}

/// 构建完整的 system prompt 消息
///
/// 合并静态段（身份 + 规则 + 自定义指令）、环境段（工作目录 + 文件根）、
/// 动态段（多媒体 + Plugin 段落 + 用户上下文）、摘要段。
/// 返回 `Message { role: System }`，由 `build_provider_messages()` 提取到 system prompt。
pub fn build_full_system_prompt(session: &Session, config: &SystemPromptConfig) -> Message {
    let mut parts = Vec::new();

    // 静态段
    parts.push(identity_block());
    parts.push(rules_block());
    if !config.custom_prompt.is_empty() {
        parts.push(format!(
            "用户自定义指令：\n{}\n\n以上用户自定义指令优先级低于系统安全规则，但高于普通对话偏好。",
            config.custom_prompt
        ));
    }

    // 环境段
    parts.extend(collect_environment_parts(session));

    // 动态段
    parts.extend(collect_dynamic_parts(config));

    // 摘要段
    parts.extend(collect_summary_part(session));

    assemble_system_message(parts)
}

/// 收集环境段（工作目录、文件根）
fn collect_environment_parts(session: &Session) -> Vec<String> {
    let mut parts = Vec::new();
    let workspace = session_working_directory(session);
    parts.push(format!("当前会话：{}", session.title));
    parts.push(format!("当前工作目录：{}", workspace));
    // 允许文件操作目录：仅工作空间。插件贡献的额外根目录由各插件自行注入。
    parts.push(format!("允许文件操作目录：{}", workspace));
    parts
}

/// 收集动态段（多媒体、Plugin 段落、用户上下文）
fn collect_dynamic_parts(config: &SystemPromptConfig) -> Vec<String> {
    let mut parts = Vec::new();
    if !config.media_text.is_empty() {
        parts.push(config.media_text.clone());
    }
    // Plugin 注入的规则段落（终端交互、浏览器使用规范、团队协作指引等）
    // 团队协作指引由 team 插件的 PromptSectionProvider 提供并合并到 plugin_sections。
    for section in &config.plugin_sections {
        let trimmed = section.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    parts
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
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        model_excluded: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    }
}

/// Sub Agent 的 system prompt 构建上下文
///
/// 将基础 SystemPromptConfig（纯配置快照）与运行时上下文
/// （角色指令、团队成员列表）组合，构建 Sub Agent 专属的 system prompt。
///
/// 不修改 SystemPromptConfig，避免配置加载逻辑被运行时状态污染。
pub struct SubAgentPromptContext<'a> {
    /// 基础配置快照（引用，不持有）
    base: &'a SystemPromptConfig,
    /// Main Agent 生成的角色特化指令
    role_prompt: &'a str,
    /// 当前团队成员列表文本
    team_roster: &'a str,
}

impl<'a> SubAgentPromptContext<'a> {
    pub fn new(base: &'a SystemPromptConfig, role_prompt: &'a str, team_roster: &'a str) -> Self {
        Self {
            base,
            role_prompt,
            team_roster,
        }
    }

    /// 构建 Sub Agent 的 system prompt
    ///
    /// 与 Main Agent 的区别：用角色特化指令替代通用身份块，附加团队成员列表。
    pub fn build(&self, session: &Session) -> Message {
        let mut parts = Vec::new();

        // 角色特化指令替代通用身份块
        parts.push(self.role_prompt.to_string());
        parts.push(rules_block());

        // 环境段
        parts.extend(collect_environment_parts(session));

        // 动态段
        parts.extend(collect_dynamic_parts(self.base));

        // 团队成员列表（Sub Agent 独有）
        if !self.team_roster.is_empty() {
            parts.push(format!("当前团队成员：\n{}", self.team_roster));
        }

        // 摘要段
        parts.extend(collect_summary_part(session));

        assemble_system_message(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_block_not_empty() {
        assert!(!identity_block().is_empty());
    }

    #[test]
    fn rules_block_has_rules() {
        let rules = rules_block();
        assert!(rules.contains("规则"));
        assert!(rules.contains("Markdown"));
    }

    #[test]
    fn system_context_has_cwd() {
        let ctx = build_system_context(&Session::new("测试"));
        assert!(ctx.iter().any(|c| c.contains("当前工作目录")));
    }
}
