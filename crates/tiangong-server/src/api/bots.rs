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
    let extra_env = server_bot_env();
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
    let extra_env = server_bot_env();
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

/// 推导 bot 回连 Server 所需的 extra_env。
///
/// host/port/token 从 Server 配置读取（阶段 3 起可从请求 body 补充覆盖）。
fn server_bot_env() -> std::collections::BTreeMap<String, String> {
    let config = tiangong_config::load_server_config();
    let mut env = std::collections::BTreeMap::new();
    env.insert(
        "TIANGONG_URL".to_string(),
        format!("http://{}:{}", config.host, config.port),
    );
    if let Some(t) = config.auth_token {
        env.insert("TIANGONG_TOKEN".to_string(), t);
    }
    env
}
