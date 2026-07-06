use std::io::{self, IsTerminal, Write};

use anyhow::Result;

use tiangong_core::agent_config::InstalledSkillConfig;
use tiangong_core::app_state::TiangongState;
use tiangong_core::skill::init_tiangong_skill_scaffold;
use tiangong_plugin_skill::SkillPlugin;

use crate::args::{SkillArgs, SkillSubcommand};

pub(crate) fn run_skill_command(args: SkillArgs) -> anyhow::Result<()> {
    let mut state = TiangongState::load_or_default();
    let skill_plugin = SkillPlugin::new();
    match args.command {
        SkillSubcommand::List => {
            println!("{}", summarize_skills(&skill_plugin.installed_skills()));
        }
        SkillSubcommand::Show { id } => {
            if let Some(id) = id {
                println!(
                    "{}",
                    describe_skills(&skill_plugin.installed_skills(), Some(&id))
                );
            } else {
                println!("{}", summarize_skills(&skill_plugin.installed_skills()));
            }
        }
        SkillSubcommand::Init {
            path,
            name,
            id,
            force,
        } => {
            let result = init_tiangong_skill_scaffold(
                std::path::Path::new(&path),
                name.as_deref(),
                id.as_deref(),
                force,
            )?;
            println!(
                "skill 初始化完成：id={} name={} path={}",
                result.skill_id,
                result.skill_name,
                result.dir.display()
            );
        }
        SkillSubcommand::Install { .. } => {
            println!(
                "固定路径安装已废弃。请在对话中让 Agent 调用 install_skill 工具自主创建 skill。"
            );
        }
        SkillSubcommand::Remove { id } => {
            let outcome = skill_plugin.remove_skill(&id)?;
            // 清理孤儿托管 MCP server
            if !outcome.orphan_mcp_servers.is_empty() {
                state
                    .store
                    .agent
                    .agent_config
                    .mcp
                    .servers
                    .retain(|s| !outcome.orphan_mcp_servers.contains(&s.name));
            }
            println!("{}", outcome.message);
        }
        SkillSubcommand::Enable { id } => {
            let msg = skill_plugin.set_skill_enabled(&id, true)?;
            println!("{msg}");
        }
        SkillSubcommand::Disable { id } => {
            let msg = skill_plugin.set_skill_enabled(&id, false)?;
            println!("{msg}");
        }
        SkillSubcommand::Refresh => {
            let msg = skill_plugin.refresh_skills()?;
            println!("{msg}");
        }
        SkillSubcommand::Gc { apply } => {
            let msg = skill_plugin.gc_skills(apply)?;
            println!("{msg}");
        }
        SkillSubcommand::Doctor => {
            let msg = skill_plugin.doctor_skills()?;
            println!("{msg}");
        }
        SkillSubcommand::Validate => {
            state.validate_agent_config()?;
            println!("配置校验通过");
        }
    }
    Ok(())
}

fn summarize_skills(skills: &[InstalledSkillConfig]) -> String {
    if skills.is_empty() {
        return "暂无已安装 skill".to_string();
    }
    let mut lines = vec![format!("已安装 skill（{}）：", skills.len())];
    for skill in skills {
        let status = if skill.enabled { "✓" } else { "✗" };
        lines.push(format!(
            "  {status} {} (id={}): {}",
            skill.name, skill.id, skill.description
        ));
    }
    lines.join("\n")
}

fn describe_skills(skills: &[InstalledSkillConfig], id_filter: Option<&str>) -> String {
    let filtered: Vec<&InstalledSkillConfig> = skills
        .iter()
        .filter(|s| id_filter.is_some_and(|id| s.id == id))
        .collect();
    if filtered.is_empty() {
        return format!("未找到匹配的 skill：{}", id_filter.unwrap_or(""));
    }
    filtered
        .iter()
        .map(|skill| {
            format!(
                "id: {}\nname: {}\nversion: {}\nenabled: {}\ndescription: {}\nentry: {}\nsource: {}",
                skill.id,
                skill.name,
                skill.version,
                skill.enabled,
                skill.description,
                skill.entry,
                skill.source.value
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}
