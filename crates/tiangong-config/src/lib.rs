//! tiangong-config：天工配置加载与管理
//!
//! 负责从磁盘读取配置文件，构建 CoreConfig 供 TiangongCore 使用。
//! CLI/GUI/Server 依赖此 crate 加载配置。
//! 第三方开发者可以不依赖此 crate，直接构造 CoreConfig。

mod loader;

pub use loader::{
    default_tiangong_dir, load_core_config, load_core_config_from_dir,
    load_core_config_from_env, load_mcp_config, load_models_config, load_skills_config,
};

// re-export core types for convenience
pub use tiangong_core::core_config::{CoreConfig, CoreConfigBuilder, CoreConfigProvider};
