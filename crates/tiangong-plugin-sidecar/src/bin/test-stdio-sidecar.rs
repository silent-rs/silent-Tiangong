//! 测试用 echo sidecar：stdio 端到端测试的协议对端。
//!
//! dispatch 规则：`echo` 操作原样回显 payload；`crash` 操作直接退出进程
//! （验证宿主侧换代重启）。

use std::sync::Arc;

use async_trait::async_trait;
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, Request, Response, ServiceStatus,
};
use tiangong_plugin_sidecar::{SidecarConfig, SidecarService, run};

struct EchoService;

#[async_trait]
impl SidecarService for EchoService {
    async fn dispatch(&self, request: Request) -> Response {
        match request.operation.as_str() {
            HANDSHAKE_OPERATION => Response::success(
                &request.request_id,
                serde_json::to_value(HandshakeResponse {
                    plugin_id: "test-stdio".to_string(),
                    plugin_version: "0.0.0".to_string(),
                    sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
                    protocol_version: tiangong_plugin_runtime::protocol::PROTOCOL_VERSION
                        .to_string(),
                    business_protocol: 0,
                    capabilities: vec!["echo".to_string()],
                    instance_id: format!("test-stdio-{}", std::process::id()),
                    status: ServiceStatus::Ready,
                })
                .expect("序列化握手响应失败"),
            ),
            "echo" => Response::success(&request.request_id, request.payload),
            "crash" => {
                std::process::exit(86);
            }
            other => Response::error(
                &request.request_id,
                ErrorCode::BadRequest,
                format!("未知操作: {other}"),
                false,
            ),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = SidecarConfig::new("test-stdio");
    run(config, || Ok(Arc::new(EchoService))).await
}
