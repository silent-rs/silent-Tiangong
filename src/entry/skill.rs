use crate::core::agent_config::InstalledSkillConfig;
use crate::core::app_state::TiangongState;

use super::args::{SkillArgs, SkillSubcommand};

pub(super) fn run_skill_command(args: SkillArgs) -> anyhow::Result<()> {
    let mut state = TiangongState::load_or_default();
    match args.command {
        SkillSubcommand::List => {
            println!("{}", summarize_skills(state.installed_skills()));
        }
        SkillSubcommand::Show { id } => {
            if let Some(id) = id {
                println!("{}", describe_skills(state.installed_skills(), Some(&id)));
            } else {
                println!("{}", summarize_skills(state.installed_skills()));
            }
        }
        SkillSubcommand::Init {
            path,
            name,
            id,
            force,
        } => {
            let msg = state.init_skill_scaffold(&path, name.as_deref(), id.as_deref(), force)?;
            println!("{msg}");
        }
        SkillSubcommand::Install {
            path,
            enabled,
            convert,
        } => {
            let msg = state.install_local_skill_with_options(&path, enabled, convert)?;
            println!("{msg}");
        }
        SkillSubcommand::Remove { id } => {
            let msg = state.remove_skill(&id)?;
            println!("{msg}");
        }
        SkillSubcommand::Enable { id } => {
            let msg = state.set_skill_enabled(&id, true)?;
            println!("{msg}");
        }
        SkillSubcommand::Disable { id } => {
            let msg = state.set_skill_enabled(&id, false)?;
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
        return "无已安装 skill".to_string();
    }
    let enabled = skills.iter().filter(|item| item.enabled).count();
    let mut lines = vec![format!(
        "skills total={} enabled={} disabled={}",
        skills.len(),
        enabled,
        skills.len() - enabled
    )];
    for item in skills {
        let status = if item.enabled { "enabled" } else { "disabled" };
        lines.push(format!(
            "- [{status}] {}@{} source={}:{}",
            item.id, item.version, item.source.kind, item.source.value
        ));
    }
    lines.join("\n")
}

fn describe_skills(skills: &[InstalledSkillConfig], id_filter: Option<&str>) -> String {
    let mut lines = Vec::new();
    let mut count = 0usize;
    for item in skills {
        if let Some(id) = id_filter
            && item.id != id
        {
            continue;
        }
        count += 1;
        let status = if item.enabled { "enabled" } else { "disabled" };
        lines.push(format!("id={}", item.id));
        lines.push(format!("name={}", item.name));
        lines.push(format!("version={}", item.version));
        lines.push(format!("status={status}"));
        lines.push(format!("entry={}", item.entry));
        lines.push(format!("source={}:{}", item.source.kind, item.source.value));
        lines.push(format!("installed_at={}", item.installed_at));
        lines.push(format!(
            "managed_mcp_servers={}",
            if item.managed_mcp_servers.is_empty() {
                "-".to_string()
            } else {
                item.managed_mcp_servers.join(",")
            }
        ));
        lines.push(format!(
            "requires_mcp={}",
            if item.requires_mcp.is_empty() {
                "-".to_string()
            } else {
                item.requires_mcp
                    .iter()
                    .map(|dep| dep.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            }
        ));
        lines.push(String::new());
    }

    if count == 0 {
        if let Some(id) = id_filter {
            return format!("未找到 skill：{id}");
        }
        return "无已安装 skill".to_string();
    }

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
