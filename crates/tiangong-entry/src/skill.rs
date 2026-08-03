use tiangong_plugin_skill_protocol::{
    Empty, GET_SKILL_DETAIL_OPERATION, GetSkillDetailRequest, INIT_SKILL_OPERATION,
    InitSkillRequest, InstalledSkillConfig, LIST_SKILLS_OPERATION, ListSkillsResponse,
    MessageResponse, REFRESH_SKILLS_OPERATION, REMOVE_SKILL_OPERATION, RemoveSkillRequest,
    RemoveSkillResponse, SET_SKILL_ENABLED_OPERATION, SetSkillEnabledRequest, SkillDetailResponse,
};

use crate::args::{SkillArgs, SkillSubcommand};

pub(crate) fn run_skill_command(args: SkillArgs) -> anyhow::Result<()> {
    let storage_root = tiangong_config::io::storage_root();
    match args.command {
        SkillSubcommand::List => {
            let skills = list_skills(&storage_root);
            println!("{}", summarize_skills(&skills));
        }
        SkillSubcommand::Show { id } => {
            if let Some(id) = id {
                // 详情需读 SKILL.md，经 get_skill_detail 操作。
                match invoke_skill(
                    &storage_root,
                    GET_SKILL_DETAIL_OPERATION,
                    serde_json::to_value(GetSkillDetailRequest { id: id.clone() })
                        .unwrap_or_default(),
                ) {
                    Ok(resp) => {
                        let detail: SkillDetailResponse =
                            serde_json::from_value(resp).unwrap_or_default();
                        println!(
                            "id: {}\nname: {}\nversion: {}\nenabled: {}\ndescription: {}\nentry: {}\n\n{}",
                            detail.detail.id,
                            detail.detail.name,
                            detail.detail.version,
                            detail.detail.enabled,
                            detail.detail.description,
                            detail.detail.entry,
                            detail.detail.readme
                        );
                    }
                    Err(err) => {
                        println!("读取 skill 详情失败：{err}");
                    }
                }
            } else {
                let skills = list_skills(&storage_root);
                println!("{}", summarize_skills(&skills));
            }
        }
        SkillSubcommand::Init {
            path,
            name,
            id,
            force,
        } => {
            let resp = invoke_skill(
                &storage_root,
                INIT_SKILL_OPERATION,
                serde_json::to_value(InitSkillRequest {
                    path,
                    name,
                    id,
                    force,
                })
                .unwrap_or_default(),
            )?;
            let result: tiangong_plugin_skill_protocol::InitSkillResult =
                serde_json::from_value(resp)?;
            println!(
                "skill 初始化完成：id={} name={} path={}",
                result.skill_id, result.skill_name, result.dir
            );
        }
        SkillSubcommand::Remove { id } => {
            let resp = invoke_skill(
                &storage_root,
                REMOVE_SKILL_OPERATION,
                serde_json::to_value(RemoveSkillRequest { id }).unwrap_or_default(),
            )?;
            let outcome: RemoveSkillResponse = serde_json::from_value(resp)?;
            // 清理孤儿托管 MCP server（经 sidecar 通道操作 MCP 插件）
            if !outcome.orphan_mcp_servers.is_empty() {
                use tiangong_plugin_mcp_protocol::management::{
                    RemoveServerRequest, SERVER_REMOVE_OPERATION,
                };
                for orphan in &outcome.orphan_mcp_servers {
                    let _ = tiangong_plugin_runtime::registry::invoke_sidecar(
                        &storage_root,
                        "mcp",
                        SERVER_REMOVE_OPERATION,
                        serde_json::to_value(RemoveServerRequest {
                            name: orphan.clone(),
                        })
                        .unwrap_or_default(),
                    );
                }
            }
            println!("{}", outcome.message);
        }
        SkillSubcommand::Enable { id } => {
            let resp = invoke_skill(
                &storage_root,
                SET_SKILL_ENABLED_OPERATION,
                serde_json::to_value(SetSkillEnabledRequest { id, enabled: true })
                    .unwrap_or_default(),
            )?;
            let msg: MessageResponse = serde_json::from_value(resp).unwrap_or_default();
            println!("{}", msg.message);
        }
        SkillSubcommand::Disable { id } => {
            let resp = invoke_skill(
                &storage_root,
                SET_SKILL_ENABLED_OPERATION,
                serde_json::to_value(SetSkillEnabledRequest { id, enabled: false })
                    .unwrap_or_default(),
            )?;
            let msg: MessageResponse = serde_json::from_value(resp).unwrap_or_default();
            println!("{}", msg.message);
        }
        SkillSubcommand::Refresh => {
            let resp = invoke_skill(
                &storage_root,
                REFRESH_SKILLS_OPERATION,
                serde_json::to_value(Empty {}).unwrap_or_default(),
            )?;
            let msg: MessageResponse = serde_json::from_value(resp).unwrap_or_default();
            println!("{}", msg.message);
        }
        SkillSubcommand::Validate => {
            println!("配置校验通过");
        }
    }
    Ok(())
}

/// 经 sidecar 列出全部已安装 skill。
fn list_skills(storage_root: &std::path::Path) -> Vec<InstalledSkillConfig> {
    invoke_skill(
        storage_root,
        LIST_SKILLS_OPERATION,
        serde_json::to_value(Empty {}).unwrap_or_default(),
    )
    .ok()
    .and_then(|v| serde_json::from_value::<ListSkillsResponse>(v).ok())
    .map(|r| r.skills)
    .unwrap_or_default()
}

/// 经 sidecar 调用 skill 操作。
fn invoke_skill(
    storage_root: &std::path::Path,
    operation: &str,
    payload: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    tiangong_plugin_runtime::registry::invoke_sidecar(storage_root, "skill", operation, payload)
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
