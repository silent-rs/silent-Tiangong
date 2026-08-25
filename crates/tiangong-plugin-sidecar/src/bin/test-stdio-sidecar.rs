//! 测试用 echo sidecar：stdio 端到端测试的协议对端。
//!
//! dispatch 规则：`echo` 操作原样回显 payload；`crash` 操作直接退出进程
//! （验证宿主侧换代重启）；Unix 下 `hang` 启动后台进程后保持请求运行。

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
            "hang" => hang_with_child(request).await,
            other => Response::error(
                &request.request_id,
                ErrorCode::BadRequest,
                format!("未知操作: {other}"),
                false,
            ),
        }
    }
}

#[cfg(unix)]
async fn hang_with_child(request: Request) -> Response {
    let sidecar_pid_file = request
        .payload
        .get("sidecar_pid_file")
        .and_then(serde_json::Value::as_str);
    let child_pid_file = request
        .payload
        .get("child_pid_file")
        .and_then(serde_json::Value::as_str);
    let (Some(sidecar_pid_file), Some(child_pid_file)) = (sidecar_pid_file, child_pid_file) else {
        return Response::error(
            &request.request_id,
            ErrorCode::BadRequest,
            "hang 缺少 PID 文件路径",
            false,
        );
    };
    let mut child = match std::process::Command::new("/bin/sleep").arg("300").spawn() {
        Ok(child) => child,
        Err(error) => {
            return Response::error(
                &request.request_id,
                ErrorCode::ServiceError,
                format!("启动测试后台进程失败: {error}"),
                false,
            );
        }
    };
    let write_result = std::fs::write(sidecar_pid_file, std::process::id().to_string())
        .and_then(|_| std::fs::write(child_pid_file, child.id().to_string()));
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Response::error(
            &request.request_id,
            ErrorCode::ServiceError,
            format!("写入测试 PID 失败: {error}"),
            false,
        );
    }
    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    let _ = child.kill();
    let _ = child.wait();
    Response::success(&request.request_id, serde_json::Value::Null)
}

#[cfg(not(unix))]
async fn hang_with_child(request: Request) -> Response {
    Response::error(
        &request.request_id,
        ErrorCode::BadRequest,
        "hang 仅用于 Unix 生命周期验证",
        false,
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = SidecarConfig::new("test-stdio");
    run(config, || Ok(Arc::new(EchoService))).await
}
