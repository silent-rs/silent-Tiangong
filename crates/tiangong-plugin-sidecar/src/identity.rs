//! sidecar 身份与配置。

use tiangong_plugin_runtime::protocol::PROTOCOL_VERSION;

/// sidecar 配置：标识一个 sidecar 实例所需的业务中立参数。
///
/// 各插件 sidecar 在启动时构造此配置，连同业务 service 一起传给 [`crate::run`]。
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// 服务名（同时也是 endpoint 文件名前缀，如 `"mcp"` / `"index"`）。
    pub service: String,
    /// 心跳线程名前缀（如 `"mcp-heartbeat"`），便于诊断时区分线程归属。
    pub heartbeat_prefix: String,
    /// 传输协议版本（写入 leader.json，用于单例身份校验：版本不匹配的旧 leader
    /// 不会阻止新候选接管）。默认取运行时 `PROTOCOL_VERSION`。
    pub protocol_version: String,
}

impl SidecarConfig {
    /// 用服务名构造默认配置（心跳前缀自动取 `{service}-heartbeat`，
    /// 协议版本取运行时 `PROTOCOL_VERSION`）。
    pub fn new(service: impl Into<String>) -> Self {
        let service = service.into();
        let heartbeat_prefix = format!("{service}-heartbeat");
        Self {
            service,
            heartbeat_prefix,
            protocol_version: PROTOCOL_VERSION.to_string(),
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
