use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use crate::auth::check_auth;

/// GET /api/v1/skills — Skill 列表
#[allow(deprecated)]
pub async fn list_skills(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;

    let skills: Vec<serde_json::Value> = app
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
