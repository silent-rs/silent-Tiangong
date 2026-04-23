use std::io::{self, IsTerminal, Write};

use anyhow::Result;

use tiangong_core::agent_config::InstalledSkillConfig;
use tiangong_core::app_state::{SkillInstallInspection, TiangongState};

use crate::args::{SkillArgs, SkillSubcommand};

pub(crate) fn run_skill_command(args: SkillArgs) -> anyhow::Result<()> {
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
            let convert_env_values = collect_conversion_env_inputs(&state, &path, convert)?;
            let msg = state.install_local_skill_with_options_and_inputs(
                &path,
                enabled,
                convert,
                &convert_env_values,
            )?;
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
        SkillSubcommand::Refresh => {
            let msg = state.refresh_skills()?;
            println!("{msg}");
        }
        SkillSubcommand::Gc { apply } => {
            let msg = state.gc_skills(apply)?;
            println!("{msg}");
        }
        SkillSubcommand::Doctor => {
            let msg = state.doctor_skills()?;
            println!("{msg}");
        }
        SkillSubcommand::Validate => {
            state.validate_agent_config()?;
            println!("配置校验通过");
        }
    }
    Ok(())
}

fn collect_conversion_env_inputs(
    state: &TiangongState,
    path: &str,
    convert: bool,
) -> Result<Vec<(String, String)>> {
    if !convert {
        return Ok(Vec::new());
    }
    let inspection = state.inspect_skill_install_requirements(path, true)?;
    print_conversion_inspection(&inspection);
    if inspection.missing_env_vars.is_empty() {
        return Ok(Vec::new());
    }
    if !io::stdin().is_terminal() {
        println!("当前非交互终端，已跳过环境变量输入步骤。");
        return Ok(Vec::new());
    }

    println!("请补充转换所需的环境变量（回车留空表示跳过）：");
    let mut values = Vec::new();
    for key in &inspection.missing_env_vars {
        print!("{key} = ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let value = line.trim().to_string();
        if value.is_empty() {
            continue;
        }
        values.push((key.clone(), value));
    }
    if values.is_empty() {
        println!("未录入新的环境变量，转换将继续执行。");
    } else {
        println!("已录入 {} 个环境变量。", values.len());
    }
    Ok(values)
}

fn print_conversion_inspection(inspection: &SkillInstallInspection) {
    if !inspection.dependencies.is_empty() {
        println!("转换分析：检测到依赖");
        for dep in &inspection.dependencies {
            println!("- {dep}");
        }
    }
    if !inspection.env_vars.is_empty() {
        println!("转换分析：检测到环境变量");
        for key in &inspection.env_vars {
            let state = if inspection.missing_env_vars.contains(key) {
                "missing"
            } else {
                "present"
            };
            println!("- {key} ({state})");
        }
    }
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
        let marker = if item.enabled {
            "\x1b[32m●\x1b[0m"
        } else {
            "\x1b[31m●\x1b[0m"
        };
        lines.push(format!(
            "{marker} {}@{} source={}:{}",
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
        let status = if item.enabled {
            "\x1b[32m● enabled\x1b[0m"
        } else {
            "\x1b[31m● disabled\x1b[0m"
        };
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
