//! 产品文案进程内插件。
//!
//! 把此前硬编码在 `tiangong-core::prompt::sections` 中的产品身份、通用回复规则
//! 与用户自定义指令外围文案，统一以插件段落形式注入 system prompt。core 只保留
//! 运行时上下文（会话标题 / 工作目录 / 摘要）与段落组装框架。
//!
//! 三宿主（CLI / Server / Desktop）与子 Core 都注册本插件，保证主 Agent 与子 Agent
//! 共用同一份基础文案。段落顺序由插件注册顺序保证——本插件应在插件列表最前注册。
//!
//! 具体工具的使用规则（如 run_shell / spawn_task）不在此处，而由对应能力插件
//! （terminal / task）各自经 `PromptSectionProvider` 注入，遵循「能力拥有者提供」。

use std::sync::{Arc, RwLock};

use tiangong_core::core::Plugin;
use tiangong_core::tool_override::PromptSectionProvider;

/// 产品文案插件。
///
/// 注入 identity（产品身份）、通用 rules（回复风格 / 格式规范）与用户自定义指令外围包装。
///
/// `custom_prompt` 在 `register` 时从 `AgentConfig.custom_system_prompt` 读取并缓存，
/// 覆盖两种场景：
/// - 主 Agent：值为宿主全局用户自定义指令（经 `to_core_config` 从 registry 落入 CoreConfig）；
/// - 子 Agent：值为「用户自定义指令 + 角色 prompt」组合（经 agent-team 插件的 `child_config` 写入）。
pub struct PromptPlugin {
    custom_prompt: RwLock<String>,
}

impl Default for PromptPlugin {
    fn default() -> Self {
        Self {
            custom_prompt: RwLock::new(String::new()),
        }
    }
}

impl PromptPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前缓存的自定义指令快照。
    fn custom_prompt_snapshot(&self) -> String {
        self.custom_prompt
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl Plugin for PromptPlugin {
    fn id(&self) -> &str {
        "prompt"
    }

    fn register(&self, engine: &tiangong_core::runtime::RuntimeEngine) {
        // 缓存当前 Core 的自定义指令：主 Agent 取宿主全局值，子 Agent 取角色 prompt。
        // engine 每次 build / rebuild 都会重新调 register，故配置变更后缓存自动刷新。
        let custom = engine.agent_config().custom_system_prompt.clone();
        if let Ok(mut guard) = self.custom_prompt.write() {
            *guard = custom;
        }
    }
}

/// 产品身份段。
fn identity_section() -> &'static str {
    "你是天工智能助手，一个功能丰富的个人 AI 中枢。你可以回答问题、处理文件、执行命令、生成多媒体内容，也可以通过工具和扩展能力完成各种复杂任务。"
}

/// 通用回复规则段。
///
/// 仅保留与具体工具名无关的通用风格 / 格式规范；涉及具体工具的使用规则由各能力
/// 插件自行注入。
fn rules_section() -> &'static str {
    "规则：\n\
     1. 对话时自然友好，回复内容完整有用。闲聊和问候时正常交流，简单介绍自己的能力。\n\
     2. 需要文件操作、代码搜索、命令执行等实际操作时，调用对应的工具。\n\
     3. 每次工具调用后会收到执行结果，根据结果决定下一步：继续调用工具或给出最终回复。\n\
     4. 执行工具任务时语言简洁高效，不要说\"让我查看\"之类的过渡语，直接给出结果。\n\
     5. 不要在回复中包含工具调用的原始痕迹（如 ok=、exit_code= 等元数据）。\n\
     6. 回复使用 Markdown 格式：代码和命令用代码块包裹，使用标题、列表等结构化排版。\n\
     7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。"
}

/// 用户自定义指令外围包装段（内容为空时返回 None）。
fn custom_prompt_section(custom: &str) -> Option<String> {
    let custom = custom.trim();
    if custom.is_empty() {
        return None;
    }
    Some(format!(
        "用户自定义指令：\n{custom}\n\n以上用户自定义指令优先级低于系统安全规则，但高于普通对话偏好。"
    ))
}

// ToolSpecProvider / ToolOverrideHandler 使用默认空实现（本插件不暴露工具，仅注入 prompt 段落）
impl tiangong_core::tool_override::ToolSpecProvider for PromptPlugin {}
impl tiangong_core::tool_override::ToolOverrideHandler for PromptPlugin {}

impl PromptSectionProvider for PromptPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        let mut sections = vec![identity_section().to_string(), rules_section().to_string()];
        if let Some(custom) = custom_prompt_section(&self.custom_prompt_snapshot()) {
            sections.push(custom);
        }
        sections
    }
}

/// 构造产品文案插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(PromptPlugin::new())
}

/// 构造默认的产品文案插件列表，供各入口（CLI / Server / Desktop）注入 core 时使用。
///
/// 应在插件列表最前注册，保证身份与规则段排在 system prompt 开头。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_section_mentions_product() {
        assert!(identity_section().contains("天工智能助手"));
    }

    #[test]
    fn rules_section_has_general_rules_without_tool_names() {
        let rules = rules_section();
        assert!(rules.starts_with("规则"));
        assert!(rules.contains("Markdown"));
        // 通用规则不应包含具体工具名（由能力插件自治）
        assert!(!rules.contains("run_shell"));
        assert!(!rules.contains("spawn_task"));
    }

    #[test]
    fn build_plugin_returns_prompt_id() {
        let plugin = build_plugin();
        assert_eq!(plugin.id(), "prompt");
    }

    #[test]
    fn custom_prompt_section_none_when_empty() {
        assert!(custom_prompt_section("").is_none());
        assert!(custom_prompt_section("   \n  ").is_none());
    }

    #[test]
    fn custom_prompt_section_wraps_content() {
        let section = custom_prompt_section("总是用简体中文").unwrap();
        assert!(section.contains("用户自定义指令"));
        assert!(section.contains("总是用简体中文"));
        assert!(section.contains("优先级低于系统安全规则"));
    }

    #[test]
    fn prompt_sections_includes_identity_rules_then_custom() {
        let plugin = PromptPlugin::new();
        // 写入自定义指令后应出现在第三段
        *plugin.custom_prompt.write().unwrap() = "测试指令".to_string();
        let sections = PromptSectionProvider::prompt_sections(&plugin);
        assert_eq!(sections.len(), 3);
        assert!(sections[0].contains("天工智能助手"));
        assert!(sections[1].starts_with("规则"));
        assert!(sections[2].contains("测试指令"));
    }
}
