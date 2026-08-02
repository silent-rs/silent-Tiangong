//! sidecar 身份与配置。

/// sidecar 配置：标识一个 sidecar 实例所需的业务中立参数。
///
/// 各插件 sidecar 在启动时构造此配置，连同业务 service 一起传给 [`crate::run`]。
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// 服务名（同时也是 endpoint 文件名前缀，如 `"mcp"` / `"index"`）。
    pub service: String,
    /// 心跳线程名前缀（如 `"mcp-heartbeat"`），便于诊断时区分线程归属。
    pub heartbeat_prefix: String,
}

impl SidecarConfig {
    /// 用服务名构造默认配置（心跳前缀自动取 `{service}-heartbeat`）。
    pub fn new(service: impl Into<String>) -> Self {
        let service = service.into();
        let heartbeat_prefix = format!("{service}-heartbeat");
        Self {
            service,
            heartbeat_prefix,
        }
    }
}

/// sidecar 实例身份（运行时生成，用于 leader.json / 日志 / 握手响应）。
#[derive(Debug, Clone)]
pub struct SidecarIdentity {
    /// 服务名。
    pub service: String,
    /// 实例标识（`{service}-sidecar-{pid}`）。
    pub instance_id: String,
}

impl SidecarIdentity {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
            instance_id: format!("{service}-sidecar-{}", std::process::id()),
        }
    }
}
