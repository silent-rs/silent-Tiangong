//! Skill 插件的 Prompt 段落实现。
//!
//! 向 system prompt 注入两段内容：
//! 1. **已安装 Skills 摘要**：引导主模型先调用 `get_skill_detail` 获取完整说明。
//! 2. **Skill 创建规范**：当用户要求创建/安装 skill 时，引导 Agent 用通用文件工具
//!    在 skills 目录下编写 `skill.toml` + `SKILL.md`（不使用专用安装工具）。

use tiangong_core::skill::read_skill_manifest;
use tiangong_core::tool_override::PromptSectionProvider;

use crate::plugin::SkillPlugin;

/// Skill 创建规范段落（始终注入，让 Agent 知道如何创建 skill）。
fn skill_creation_guide(root: &std::path::Path) -> String {
    format!(
        r#"Skill 创建规范（当用户要求创建/安装/编写 Skill 时遵循）：
- Skill 存储目录：{root}
- 创建步骤：
  1. 在存储目录下创建子目录 `<skill-id>/`（id 用小写字母、数字、中横线，与 skill.toml 的 id 一致）
  2. 写入 `skill.toml`（manifest，最小模板见下）
  3. 写入 `SKILL.md`（使用说明，首行 `# <标题>`）
- skill.toml 最小模板：
  id = "<skill-id>"
  name = "<显示名>"
  version = "0.1.0"
  entry = "SKILL.md"
  available = true

  [source]
  type = "local"
  value = ""

  [requires]
  mcp = []

  [permissions]
  fs_read = []
  fs_write = []
  cmd_exec = []
  net = []
- SKILL.md 支持 `{{skill_dir}}` 占位符（运行时替换为 skill 目录绝对路径）
- 创建完成后提示用户：刷新 Skill 列表或开启新对话后生效
- 不要调用任何专用安装工具；直接用文件工具（write_file 等）创建上述文件"#,
        root = root.display()
    )
}

impl PromptSectionProvider for SkillPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        let registry = self.registry();
        let root = registry.root().to_path_buf();
        let view = registry.view();

        let mut sections = Vec::new();

        // 1. 已安装 Skills 摘要
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
        if !summaries.is_empty() {
            sections.push(format!(
                "已安装的 Skills（使用前先调用 get_skill_detail 获取完整说明）：\n{}",
                summaries.join("\n")
            ));
        }

        // 2. Skill 创建规范（始终注入）
        sections.push(skill_creation_guide(&root));

        sections
    }
}
