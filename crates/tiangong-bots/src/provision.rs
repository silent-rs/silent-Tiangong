//! 扫码授权配置（通用扫码能力）。
//!
//! 飞书的"扫码创建应用"基于 `accounts.feishu.cn/oauth/v1/app/registration` 的
//! `init → begin → poll` 三步协议：
//! - `init`：返回支持的认证方式。
//! - `begin`：返回 `verification_uri_complete`（扫码 URL）+ `device_code` + 过期/间隔。
//! - `poll`：轮询直到用户授权，成功返回 `client_id` + `client_secret`。
//!
//! 参见 `oapi-sdk-python/lark_oapi/scene/registration/__init__.py`。
//!
//! 本模块定义 [`BotProvisioner`] trait 作为通用扫码抽象——任何声明了
//! [`crate::config::FieldType::Barcode`] 字段的 bot 都可通过对应的
//! provisioner 扫码获取凭证。首期仅定义 trait 与类型，飞书 impl 留后续迭代。

use std::collections::BTreeMap;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// 扫码会话（begin 阶段产出）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QrSession {
    /// 扫码 URL（前端渲染为二维码）。
    pub qr_url: String,
    /// 设备码（poll 用）。
    pub device_code: String,
    /// 过期时间戳（Unix 秒）。
    pub expires_at: i64,
    /// 轮询间隔（秒）。
    pub interval: u64,
}

/// 轮询扫码授权的状态。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProvisionStatus {
    /// 等待用户扫码授权。
    Pending,
    /// 授权成功，回填的凭证（字段 key → value）。
    Success {
        credentials: BTreeMap<String, Value>,
    },
    /// 扫码会话已过期。
    Expired,
    /// 授权失败。
    Error { message: String },
}

/// 扫码授权 trait——按 bot 平台实现各自的扫码协议。
///
/// 首期为框架预留接口；飞书实现（init/begin/poll）作为后续迭代。
#[async_trait]
pub trait BotProvisioner: Send + Sync {
    /// 该 provisioner 支持的制品 id（标识 bot 平台）。
    fn artifact_id(&self) -> &str;

    /// 发起扫码，返回二维码会话供前端渲染。
    async fn begin(&self) -> Result<QrSession>;

    /// 轮询扫码状态（前端按 `QrSession::interval` 周期调用）。
    async fn poll(&self, session: &QrSession) -> Result<ProvisionStatus>;
}

/// 按制品 id 取对应的扫码 provisioner（首期返回 None）。
pub fn provisioner_for(_artifact_id: &str) -> Option<Box<dyn BotProvisioner>> {
    // TODO(后续迭代): 为飞书实现 FeishuProvisioner（accounts.feishu.cn 协议）。
    None
}
