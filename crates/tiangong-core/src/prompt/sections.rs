//! System Prompt Sections
//!
//! 静态块和动态块的具体内容。
//! 静态块跨会话稳定，动态块按会话/配置变化。

use crate::agent_config::AgentConfig;
use crate::models_config::ModelsConfig;
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};

/// 身份块
fn identity_block() -> String {
    "你是天工智能助手，一个功能丰富的个人 AI 中枢。你可以回答问题、处理文件、执行命令、生成多媒体内容，也可以通过工具和技能完成各种复杂任务。".to_string()
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
8. 如果已安装的 Skill 能处理用户请求，优先通过 run_command 调用 Skill 脚本。
9. 命令执行默认使用 run_shell 或 run_command，并根据工具结果继续推进。
10. 只有用户明确要求后台、不阻塞、并行、持续运行、启动服务/监听，或需要管理已有后台任务时，才使用 spawn_task / wait_tasks。"
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

/// Skills 摘要 section
fn build_skills_section(agent_config: &AgentConfig) -> String {
    let mut summaries = Vec::new();
    for skill in &agent_config.skills.installed {
        if !skill.enabled {
            continue;
        }
        summaries.push(format!(
            "- {} (id={}): {}",
            skill.name,
            skill.id,
            if skill.description.is_empty() {
                "无描述"
            } else {
                &skill.description
            }
        ));
    }
    if summaries.is_empty() {
        return String::new();
    }
    format!(
        "已安装的 Skills（使用前先调用 get_skill_detail 获取完整说明）：\n{}",
        summaries.join("\n")
    )
}

/// 构建 System Context（环境事实，追加到 system prompt 尾部）
pub fn build_system_context(session: &Session) -> Vec<String> {
    let mut ctx = Vec::new();

    let workspace = session_working_directory(session);
    ctx.push(format!("当前工作目录：{}", workspace));

    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![workspace];
    if !home.is_empty() {
        roots.push(format!("{home}/.tiangong/skills"));
    }
    ctx.push(format!("允许文件操作目录：{}", roots.join(", ")));

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

/// 团队协作工具使用指引
fn build_agent_team_section() -> String {
    "团队协作能力（可选使用）：
当任务复杂需要分工时，你可以创建团队来协作完成。以下是可用的团队工具：

- create_agent(role, label, system_prompt, tools)：创建一个 Sub Agent
  - role：角色标识（如 pm、dev、test），用于消息路由
  - label：显示名称（如「项目经理」「开发者」）
  - system_prompt：Agent 的专属指令
  - tools：可选，指定可用工具列表（默认继承你的工具集，不含 create_agent/dismiss_agent）
  - Agent 持续存在直到被解散
  - 最多同时 8 个 Agent

- send_message(to, content)：向指定角色的 Agent 发送消息，Agent 会自动执行任务
- broadcast_message(content, exclude)：向所有 Agent 广播消息
- notify_user(content, level)：向用户推送通知（info/warning/error）
- dismiss_agent(role)：解散指定角色的 Agent，释放所有资源

- lock_file(path) / unlock_file(path)：文件编辑锁，防止多 Agent 同时编辑同一文件
  - 编辑文件前建议先获取锁，编辑完成后释放锁

工作模式：
1. 你作为主 Agent，负责理解用户需求、规划任务、分配工作
2. 通过 create_agent 创建需要的角色，通过 send_message 分配具体任务
3. Sub Agent 执行完毕后结果会自动回传给你
4. 你汇总所有 Sub Agent 的结果后回复用户

注意：简单任务不需要创建团队，直接使用工具完成即可。仅在任务确实需要并行分工时使用。"
        .to_string()
}

/// 构建 system prompt 所需的动态配置数据快照
///
/// 由调用方从 AgentConfig / ModelsConfig 构建，传入 session.rebuild_system_prompt()。
pub struct SystemPromptConfig {
    pub custom_prompt: String,
    pub skills_text: String,
    pub media_text: String,
    pub team_text: String,
    /// Plugin 注入的额外段落（如终端交互引导、浏览器使用规范等）
    pub plugin_sections: Vec<String>,
    /// 文档附件解析规则段（PDF/Office 处理引导，issue #149）
    pub attachment_rules_text: String,
}

impl SystemPromptConfig {
    /// 从配置实例构建动态数据快照
    pub fn from_configs(models_config: &ModelsConfig, agent_config: &AgentConfig) -> Self {
        Self {
            custom_prompt: agent_config.custom_system_prompt.trim().to_string(),
            skills_text: build_skills_section(agent_config),
            media_text: build_media_section(models_config),
            team_text: build_agent_team_section(),
            plugin_sections: Vec::new(),
            attachment_rules_text: super::attachment_rules::attachment_rules_section(),
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
/// 动态段（多媒体 + Skills + 团队协作 + 用户上下文）、摘要段。
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
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![workspace];
    if !home.is_empty() {
        roots.push(format!("{home}/.tiangong/skills"));
    }
    parts.push(format!("允许文件操作目录：{}", roots.join(", ")));
    parts
}

/// 收集动态段（多媒体、Skills、团队协作、用户上下文）
fn collect_dynamic_parts(config: &SystemPromptConfig) -> Vec<String> {
    let mut parts = Vec::new();
    if !config.media_text.is_empty() {
        parts.push(config.media_text.clone());
    }
    if !config.attachment_rules_text.is_empty() {
        parts.push(config.attachment_rules_text.clone());
    }
    if !config.skills_text.is_empty() {
        parts.push(config.skills_text.clone());
    }
    if !config.team_text.is_empty() {
        parts.push(config.team_text.clone());
    }
    // Plugin 注入的规则段落（终端交互、浏览器使用规范等）
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
        media: Vec::new(),
        media_migrated: true,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
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
