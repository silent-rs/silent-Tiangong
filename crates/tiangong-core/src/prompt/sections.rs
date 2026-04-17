//! System Prompt Sections
//!
//! 静态块和动态块的具体内容。
//! 静态块跨会话稳定，动态块按会话/配置变化。

use crate::agent_config::AgentConfig;
use crate::models_config::ModelsConfig;

use super::types::{PromptSection, SystemPromptBlock};

/// 构建完整的 System Prompt Block
pub fn build_system_prompt_block(
    models_config: &ModelsConfig,
    agent_config: &AgentConfig,
) -> SystemPromptBlock {
    let mut block = SystemPromptBlock::new();

    // === 静态块（极少变化，缓存友好）===
    block.static_blocks.push(identity_block());
    block.static_blocks.push(rules_block());

    // === 动态块（按配置变化）===
    let dynamic_sections = build_dynamic_sections(models_config, agent_config);
    for section in dynamic_sections {
        if !section.content.is_empty() {
            block.dynamic_blocks.push(section.content);
        }
    }

    block
}

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
9. 耗时较长的命令使用 spawn_task 在后台执行。
10. 多个可并行的耗时任务使用 spawn+join 模式。"
        .to_string()
}

/// 构建动态 sections
fn build_dynamic_sections(
    models_config: &ModelsConfig,
    agent_config: &AgentConfig,
) -> Vec<PromptSection> {
    let mut sections = Vec::new();

    // 多媒体能力
    let media_section = build_media_section(models_config);
    if !media_section.is_empty() {
        sections.push(PromptSection {
            name: "media_capabilities".into(),
            content: media_section,
            cached: true,
        });
    }

    // 已安装 Skills 摘要（仅名称+描述，不注入完整 SKILL.md）
    let skills_section = build_skills_section(agent_config);
    if !skills_section.is_empty() {
        sections.push(PromptSection {
            name: "installed_skills".into(),
            content: skills_section,
            cached: true,
        });
    }

    sections
}

/// 多媒体能力 section
fn build_media_section(models_config: &ModelsConfig) -> String {
    use crate::models_config::ModelCapability;

    let mut hints = Vec::new();
    for cap in ModelCapability::media_capabilities() {
        if let Some(resolved) = models_config.resolve_for_capability(*cap) {
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
pub fn build_system_context() -> Vec<String> {
    let mut ctx = Vec::new();

    ctx.push(format!(
        "当前工作目录：{}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".into())
    ));

    let workspace = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let home = std::env::var("HOME").unwrap_or_default();
    let mut roots = vec![workspace];
    if !home.is_empty() {
        roots.push(format!("{home}/.tiangong"));
    }
    ctx.push(format!("允许文件操作目录：{}", roots.join(", ")));

    ctx
}

/// 构建 User Context（用户偏好/记忆，以 system-reminder 方式注入）
///
/// 通过 tiangong-memory 的三级 Injection 层（Profile / Workspace / Session）读取注入文件。
pub fn build_user_context(session_id: &str, workspace_id: Option<&str>) -> Vec<String> {
    tiangong_memory::load_injection_sync(session_id, workspace_id)
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
        let ctx = build_system_context();
        assert!(ctx.iter().any(|c| c.contains("当前工作目录")));
    }
}
