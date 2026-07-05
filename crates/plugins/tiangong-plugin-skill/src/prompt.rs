//! Skill 详情插件的 Prompt 段落实现。
//!
//! 向 system prompt 注入「已安装的 Skills」段落，引导主模型先调用 `get_skill_detail`
//! 获取完整说明。从自托管 [`SkillRegistry`] 扫描 available 的 skill 构造摘要。

use tiangong_core::skill::read_skill_manifest;
use tiangong_core::tool_override::PromptSectionProvider;

use crate::plugin::SkillPlugin;

impl PromptSectionProvider for SkillPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        let view = self.registry().view();
        let mut summaries = Vec::new();
        for entry in view.entries.values() {
            let Ok(manifest) = read_skill_manifest(&entry.dir.join("skill.toml")) else {
                continue;
            };
            if !manifest.available {
                continue;
            }
            summaries.push(format!(
                "- {} (id={}): {}",
                manifest.name,
                manifest.id,
                if manifest.description.is_empty() {
                    "无描述"
                } else {
                    &manifest.description
                }
            ));
        }
        if summaries.is_empty() {
            return Vec::new();
        }
        vec![format!(
            "已安装的 Skills（使用前先调用 get_skill_detail 获取完整说明）：\n{}",
            summaries.join("\n")
        )]
    }
}
