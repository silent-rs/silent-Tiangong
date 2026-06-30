//! Server 模式配置。
//!
//! 历史上此模块自定义义了与 `tiangong_config::ServerConfig` 同名的结构体，
//! 造成两套并存的 ServerConfig（技术债）。自 0.12.0（RFC 0015）起统一到
//! `tiangong_config` 版本，本模块仅做 re-export 以保持 `crate::config::`
//! 路径向后兼容。

pub use tiangong_config::{
    ServerConfig, generate_token, load_server_config, save_server_config, server_config_path,
};
