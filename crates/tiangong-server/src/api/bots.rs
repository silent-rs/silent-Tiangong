//! Bot 管理 HTTP API（issue #286 阶段 2c）。
//!
//! 暴露 Bot 生命周期读 + 启停端点，CLI 经此操作 Server 管理的 bot。镜像 jobs/sessions
//! 的 handler 模式：鉴权 → 提取 SharedAppContext → 调 bot_store/bot_runtime → JSON。
//!
//! 配置/扫码/安装/升级端点在阶段 3/4 补充。

use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use crate::auth::check_auth;
use tiangong_bots::{BotConfig, BotHealth};

/// Bot 列表项：配置 + 健康状态（对齐 CLI cmd_list 输出）。
#[derive(serde::Serialize)]
struct BotListItem {
    #[serde(flatten)]
    config: BotConfig,
    health: BotHealth,
}

/// GET /api/v1/bots — Bot 列表（含健康状态）
#[allow(deprecated)]
pub async fn list_bots(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bots = app_ctx.bot_store.list();
    let runtime = app_ctx.bot_runtime.clone();
    let mut items = Vec::with_capacity(bots.len());
    for bot in bots {
        let health = runtime.health(&bot.id).await;
        items.push(BotListItem {
            config: bot,
            health,
        });
    }
    Ok(Response::json(&serde_json::json!({
        "total": items.len(),
        "items": items,
    })))
}

/// GET /api/v1/bots/<id> — Bot 详情（配置 + 健康）
#[allow(deprecated)]
pub async fn get_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let bot = app_ctx.bot_store.get(&bot_id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Bot '{id}' 不存在"))
    })?;
    let health = app_ctx.bot_runtime.health(&bot_id).await;
    Ok(Response::json(&BotListItem {
        config: bot,
        health,
    }))
}

/// GET /api/v1/bots/<id>/health — Bot 健康状态
#[allow(deprecated)]
pub async fn get_bot_health(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let health = app_ctx.bot_runtime.health(&bot_id).await;
    Ok(Response::json(&health))
}

/// GET /api/v1/bots/<id>/logs — Bot 日志尾部
#[allow(deprecated)]
pub async fn get_bot_logs(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let bot_id = parse_bot_id(&id)?;
    let log = tiangong_bots::read_log_tail(&bot_id).map_err(|e| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("读取 Bot 日志失败：{e}"))
    })?;
    Ok(Response::json(&log))
}

/// POST /api/v1/bots/<id>/start — 启动 Bot
#[allow(deprecated)]
pub async fn start_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let bot = app_ctx.bot_store.get(&bot_id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Bot '{id}' 不存在"))
    })?;
    // extra_env 由 Server URL/Token 推导（与 start_enabled 一致），供 bot 回连。
    // TODO 阶段 3：从请求 body 接收额外 env；当前用 Server 自身配置。
    let extra_env = app_ctx.bot_connect.to_bot_env();
    app_ctx
        .bot_runtime
        .start(&bot, &extra_env)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("启动 Bot 失败：{e}"),
            )
        })?;
    Ok(Response::json(&serde_json::json!({ "status": "started" })))
}

/// POST /api/v1/bots/<id>/stop — 停止 Bot
#[allow(deprecated)]
pub async fn stop_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    app_ctx.bot_runtime.stop(&bot_id).await.map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("停止 Bot 失败：{e}"),
        )
    })?;
    Ok(Response::json(&serde_json::json!({ "status": "stopped" })))
}

/// POST /api/v1/bots/<id>/restart — 重启 Bot（stop + start）
#[allow(deprecated)]
pub async fn restart_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let bot = app_ctx.bot_store.get(&bot_id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Bot '{id}' 不存在"))
    })?;
    let runtime = app_ctx.bot_runtime.clone();
    // stop 忽略错误（可能未运行），再 start。
    let _ = runtime.stop(&bot_id).await;
    let extra_env = app_ctx.bot_connect.to_bot_env();
    runtime.start(&bot, &extra_env).await.map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("重启 Bot 失败：{e}"),
        )
    })?;
    Ok(Response::json(
        &serde_json::json!({ "status": "restarted" }),
    ))
}

/// 解析路径参数为 BotId。
fn parse_bot_id(raw: &str) -> Result<tiangong_bots::BotId> {
    tiangong_bots::BotId::try_from(raw).map_err(|err: tiangong_bots::InvalidBotId| {
        SilentError::business_error(StatusCode::BAD_REQUEST, format!("Bot ID 非法：{err}"))
    })
}

// ── 配置与扫码（issue #286 阶段 3）──

/// GET /api/v1/bots/<id>/schema — Bot 配置字段 schema
#[allow(deprecated)]
pub async fn get_bot_schema(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let bot_id = parse_bot_id(&id)?;
    // 优先返回缓存 schema；无缓存则尝试从制品提取并缓存。
    let schema = match tiangong_bots::cached_schema(&bot_id) {
        Some(s) => s,
        None => tiangong_bots::describe_and_cache(&bot_id)
            .await
            .map_err(|e| {
                SilentError::business_error(
                    StatusCode::NOT_FOUND,
                    format!("获取 Bot 配置 schema 失败（制品可能未安装）：{e}"),
                )
            })?,
    };
    Ok(Response::json(&schema))
}

/// POST /api/v1/bots — 注册新 Bot
#[allow(deprecated)]
pub async fn register_bot(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let body: tiangong_bots::RegisterBotRequest = req.json_parse().await?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot = app_ctx.bot_store.register(body).map_err(|e| {
        SilentError::business_error(StatusCode::BAD_REQUEST, format!("注册 Bot 失败：{e}"))
    })?;
    Ok(Response::json(&bot).with_status(StatusCode::CREATED))
}

/// PUT /api/v1/bots/<id>/config — 更新 Bot 配置
#[allow(deprecated)]
pub async fn update_bot_config(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let body: tiangong_bots::UpdateBotRequest = req.json_parse().await?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let bot = app_ctx.bot_store.update(&bot_id, body).map_err(|e| {
        SilentError::business_error(StatusCode::BAD_REQUEST, format!("更新 Bot 配置失败：{e}"))
    })?;
    Ok(Response::json(&bot))
}

/// DELETE /api/v1/bots/<id> — 删除 Bot（先停止再移除配置）
#[allow(deprecated)]
pub async fn delete_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    // 先停止运行中的 bot（忽略错误：可能未运行）。
    let _ = app_ctx.bot_runtime.stop(&bot_id).await;
    app_ctx.bot_store.remove(&bot_id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("删除 Bot 失败：{e}"),
        )
    })?;
    Ok(Response::json(&serde_json::json!({ "status": "deleted" })))
}

/// POST /api/v1/bots/<id>/provision/begin — 开始扫码配置
#[allow(deprecated)]
pub async fn provision_begin(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let session = app_ctx
        .bot_runtime
        .provision_begin(&bot_id)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("开始扫码配置失败：{e}"),
            )
        })?;
    Ok(Response::json(&session))
}

/// POST /api/v1/bots/<id>/provision/poll — 轮询扫码状态
#[allow(deprecated)]
pub async fn provision_poll(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let session: tiangong_bots::QrSession = req.json_parse().await?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let status = app_ctx
        .bot_runtime
        .provision_poll(&bot_id, &session)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("轮询扫码状态失败：{e}"),
            )
        })?;
    Ok(Response::json(&status))
}

// ── 安装与升级（issue #286 阶段 4）──

/// GET /api/v1/bots/available — 线上可安装制品索引
#[allow(deprecated)]
pub async fn list_available(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let index = app_ctx.bot_runtime.fetch_index().await.map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("拉取线上制品索引失败：{e}"),
        )
    })?;
    Ok(Response::json(&index))
}

/// GET /api/v1/bots/check-update?artifact_id=… — 检查更新
#[allow(deprecated)]
pub async fn check_update(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let artifact_id: String = req.get_path_params("artifact_id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let manifest = app_ctx
        .bot_runtime
        .check_update(&artifact_id)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("检查更新失败：{e}"),
            )
        })?;
    Ok(Response::json(&manifest))
}

/// POST /api/v1/bots/install — 安装制品
#[allow(deprecated)]
pub async fn install_bot(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    #[derive(serde::Deserialize)]
    struct InstallReq {
        artifact_id: String,
        id: Option<String>,
        version: Option<String>,
    }
    let body: InstallReq = req.json_parse().await?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();

    // 取线上 manifest（按可选 version 选择），下载安装到 dest_id。
    let index = app_ctx.bot_runtime.fetch_index().await.map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("拉取制品索引失败：{e}"),
        )
    })?;
    let manifest = pick_manifest(&index, &body.artifact_id, body.version.as_deref())
        .map_err(|e| SilentError::business_error(StatusCode::NOT_FOUND, e.to_string()))?;
    let dest_id_str = body.id.unwrap_or_else(|| body.artifact_id.clone());
    let dest_id = parse_bot_id(&dest_id_str)?;
    app_ctx
        .bot_runtime
        .install(manifest, &dest_id, None)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("安装制品失败：{e}"),
            )
        })?;
    Ok(Response::json(&serde_json::json!({
        "status": "installed",
        "id": dest_id_str,
    })))
}

/// POST /api/v1/bots/<id>/upgrade — 升级 bot
#[allow(deprecated)]
pub async fn upgrade_bot(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let bot_id = parse_bot_id(&id)?;
    let bot = app_ctx.bot_store.get(&bot_id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Bot '{id}' 不存在"))
    })?;
    // 检查并取得最新 manifest，失败恢复由 BotRuntime::upgrade 内部处理（stop→install）。
    let manifest = app_ctx
        .bot_runtime
        .check_update(&bot.artifact_id)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("检查更新失败：{e}"),
            )
        })?
        .ok_or_else(|| {
            SilentError::business_error(
                StatusCode::NOT_FOUND,
                format!("未找到制品 {} 的更新", bot.artifact_id),
            )
        })?;
    app_ctx
        .bot_runtime
        .upgrade(&bot_id, manifest, None)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("升级 Bot 失败：{e}"),
            )
        })?;
    Ok(Response::json(&serde_json::json!({ "status": "upgraded" })))
}

/// 从 bots-index 中按 artifact_id（及可选版本）选取 manifest。
fn pick_manifest(
    index: &tiangong_bots::manifest::BotsIndex,
    artifact_id: &str,
    version: Option<&str>,
) -> std::result::Result<tiangong_bots::manifest::BotManifest, String> {
    let mut candidates: Vec<_> = index.bots.iter().filter(|m| m.id == artifact_id).collect();
    if candidates.is_empty() {
        return Err(format!("线上索引中未找到制品：{artifact_id}"));
    }
    if let Some(ver) = version {
        candidates.retain(|m| m.version == ver);
        if candidates.is_empty() {
            return Err(format!("制品 {artifact_id} 无版本 {ver}"));
        }
    }
    // 取候选中版本最高者。
    candidates.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(candidates[0].clone())
}
