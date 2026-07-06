use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;

/// GET /api/v1/skills — Skill 列表
///
/// Skill 数据由 skill plugin 自管（~/.tiangong/skills/），server 无状态读取：
/// 每次请求构造临时 plugin 实例扫描 registry（文件共享，多进程一致）。
#[allow(deprecated)]
pub async fn list_skills(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let skill_plugin = tiangong_plugin_skill::SkillPlugin::new();
    let skills: Vec<serde_json::Value> = skill_plugin
        .installed_skills()
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "enabled": s.enabled,
            })
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "total": skills.len(),
        "items": skills,
    })))
}
