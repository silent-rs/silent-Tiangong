//! Server HTTP client for Bot management (issue #286 阶段 2d)。
//!
//! CLI 经此调用独立 Server 的 /api/v1/bots/* 端点，不再直接 spawn bot 进程。
//! base_url / token 从 server.json 配置读取。复用 tiangong-entry 已有的 reqwest 依赖。

use anyhow::{Context, Result, anyhow};
use tiangong_bots::{BotConfig, BotHealth, BotLog};
use tiangong_config::load_server_config;

/// Bot 列表项（对齐 Server api/bots.rs BotListItem）。
#[derive(serde::Deserialize)]
pub struct BotListItem {
    #[serde(flatten)]
    pub config: BotConfig,
    pub health: BotHealth,
}

/// Server Bot 管理 HTTP 客户端。
pub struct BotClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::blocking::Client,
}

impl BotClient {
    /// 从 server.json 配置构造。
    pub fn from_config() -> Result<Self> {
        let config = load_server_config();
        let base_url = format!("http://{}:{}", config.host, config.port);
        Ok(Self {
            base_url,
            token: config.auth_token,
            http: reqwest::blocking::Client::builder()
                .build()
                .context("构造 HTTP client 失败")?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// 探测 Server 是否可达（GET /api/v1/health）。
    pub fn is_reachable(&self) -> bool {
        self.http
            .get(self.url("/api/v1/health"))
            .bearer(&self.token)
            .send()
            .is_ok_and(|resp| resp.status().is_success())
    }

    /// GET /api/v1/bots — Bot 列表。
    pub fn list_bots(&self) -> Result<Vec<BotListItem>> {
        let resp = self
            .http
            .get(self.url("/api/v1/bots"))
            .bearer(&self.token)
            .send()
            .context("请求 Bot 列表失败")?;
        decode_list(resp)
    }

    /// GET /api/v1/bots/{id} — Bot 详情。
    pub fn get_bot(&self, id: &str) -> Result<BotListItem> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/bots/{id}")))
            .bearer(&self.token)
            .send()
            .context("请求 Bot 详情失败")?;
        decode(resp)
    }

    /// GET /api/v1/bots/{id}/health — 健康状态。
    pub fn get_health(&self, id: &str) -> Result<BotHealth> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/bots/{id}/health")))
            .bearer(&self.token)
            .send()
            .context("请求 Bot 健康状态失败")?;
        decode(resp)
    }

    /// GET /api/v1/bots/{id}/logs — 日志尾部。
    pub fn get_logs(&self, id: &str) -> Result<BotLog> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/bots/{id}/logs")))
            .bearer(&self.token)
            .send()
            .context("请求 Bot 日志失败")?;
        decode(resp)
    }

    /// POST /api/v1/bots/{id}/{action} — start/stop/restart。
    pub fn post_action(&self, id: &str, action: &str) -> Result<()> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v1/bots/{id}/{action}")))
            .bearer(&self.token)
            .send()
            .with_context(|| format!("{action} Bot 请求失败"))?;
        ensure_success(resp)
    }
    /// GET /api/v1/bots/{id}/schema — 配置字段 schema。
    pub fn get_schema(&self, id: &str) -> Result<Vec<tiangong_bots::ConfigFieldSchema>> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/bots/{id}/schema")))
            .bearer(&self.token)
            .send()
            .context("请求 Bot schema 失败")?;
        decode(resp)
    }

    /// POST /api/v1/bots — 注册新 bot。
    pub fn register_bot(
        &self,
        req: &tiangong_bots::RegisterBotRequest,
    ) -> Result<tiangong_bots::BotConfig> {
        let resp = self
            .http
            .post(self.url("/api/v1/bots"))
            .bearer(&self.token)
            .json(req)
            .send()
            .context("注册 Bot 请求失败")?;
        decode(resp)
    }

    /// PUT /api/v1/bots/{id}/config — 更新 bot 配置。
    pub fn update_config(
        &self,
        id: &str,
        req: &tiangong_bots::UpdateBotRequest,
    ) -> Result<tiangong_bots::BotConfig> {
        let resp = self
            .http
            .put(self.url(&format!("/api/v1/bots/{id}/config")))
            .bearer(&self.token)
            .json(req)
            .send()
            .context("更新 Bot 配置请求失败")?;
        decode(resp)
    }

    /// DELETE /api/v1/bots/{id} — 删除 bot。
    pub fn delete_bot(&self, id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(&format!("/api/v1/bots/{id}")))
            .bearer(&self.token)
            .send()
            .context("删除 Bot 请求失败")?;
        ensure_success(resp)
    }

    /// POST /api/v1/bots/{id}/provision/begin — 开始扫码配置。
    pub fn provision_begin(&self, id: &str) -> Result<tiangong_bots::QrSession> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v1/bots/{id}/provision/begin")))
            .bearer(&self.token)
            .send()
            .context("开始扫码配置请求失败")?;
        decode(resp)
    }

    /// POST /api/v1/bots/{id}/provision/poll — 轮询扫码状态。
    pub fn provision_poll(
        &self,
        id: &str,
        session: &tiangong_bots::QrSession,
    ) -> Result<tiangong_bots::ProvisionStatus> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v1/bots/{id}/provision/poll")))
            .bearer(&self.token)
            .json(session)
            .send()
            .context("轮询扫码状态请求失败")?;
        decode(resp)
    }
}

trait Bearer {
    fn bearer(self, token: &Option<String>) -> Self;
}

impl Bearer for reqwest::blocking::RequestBuilder {
    fn bearer(self, token: &Option<String>) -> Self {
        match token {
            Some(t) => self.bearer_auth(t),
            None => self,
        }
    }
}

fn ensure_success(resp: reqwest::blocking::Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = resp.text().unwrap_or_default();
        Err(anyhow!("Server 返回 {status}: {body}"))
    }
}

fn decode<T: for<'de> serde::Deserialize<'de>>(resp: reqwest::blocking::Response) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(anyhow!("Server 返回 {status}: {body}"));
    }
    resp.json::<T>().context("解析 Server 响应失败")
}

fn decode_list(resp: reqwest::blocking::Response) -> Result<Vec<BotListItem>> {
    #[derive(serde::Deserialize)]
    struct ListResp {
        items: Vec<BotListItem>,
    }
    let parsed: ListResp = decode(resp)?;
    Ok(parsed.items)
}
