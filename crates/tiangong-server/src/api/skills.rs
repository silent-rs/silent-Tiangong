use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;

/// GET /api/v1/skills — Skill 列表
///
/// Skill 数据经 skill sidecar 查询（~/.tiangong/skills/），server 无状态读取。
pub async fn list_skills(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let storage_root = tiangong_config::io::storage_root();
    let skills: Vec<serde_json::Value> = match tiangong_plugin_runtime::registry::invoke_sidecar(
        &storage_root,
        "skill",
        tiangong_plugin_skill_protocol::LIST_SKILLS_OPERATION,
        serde_json::to_value(tiangong_plugin_skill_protocol::Empty {}).unwrap_or_default(),
    ) {
        Ok(resp) => {
            let resp: tiangong_plugin_skill_protocol::ListSkillsResponse =
                serde_json::from_value(resp).unwrap_or_default();
            resp.skills
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "name": s.name,
                        "enabled": s.enabled,
                    })
                })
                .collect()
        }
        Err(_) => Vec::new(),
    };

    Ok(Response::json(&serde_json::json!({
        "total": skills.len(),
        "items": skills,
    })))
}
