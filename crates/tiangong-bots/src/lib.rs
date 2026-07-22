//! 天工通讯网关框架——bot 制品下载、进程监督与配置管理。
//!
//! 设计参见 issue #250 与 `docs/rfc/0004` 的"外部适配程序"方针
//! （`requirements.md` 明确不在天工内部实现 Connector 机制，bot 作为独立
//! 制品由主程序运行时下载、启动并监控）。
//!
//! 核心组件：
//! - [`config`]：bot 实例、字段 schema 与扫码字段抽象（[`FieldType::Barcode`]）。
//! - [`store`] / [`management`]：bots.json 持久化与 copy-on-write CRUD。
//! - [`manifest`]：bots-index.json 制品清单与平台标识。
//! - [`downloader`]：制品下载 + SHA256 校验。
//! - [`supervisor`]：进程 spawn + 崩溃重启 + PID + 日志 tail。
//! - [`runtime`]：按 bot 实例启停的运行时表。
//! - [`provision`]：扫码授权 trait（首期 stub）。

pub mod config;
pub mod downloader;
pub mod management;
pub mod manifest;
pub mod paths;
pub mod provision;
pub mod store;
pub mod supervisor;

pub use config::{
    BotConfig, BotsConfig, ConfigFieldSchema, FieldType, RegisterBotRequest, UpdateBotRequest,
};
pub use downloader::{Downloader, ProgressFn};
pub use management::BotStore;
pub use manifest::{BotArtifact, BotManifest, BotsIndex, current_platform_key};
pub use provision::{BotProvisioner, ProvisionStatus, QrSession, provisioner_for};
pub use runtime::{BotHealth, BotRuntime, bot_env, cached_schema, describe_and_cache};
pub use store::{AuditEntry, append_audit_log, atomic_write, load_bots_config, write_bots_config};
pub use supervisor::SupervisedBot;

mod runtime;
