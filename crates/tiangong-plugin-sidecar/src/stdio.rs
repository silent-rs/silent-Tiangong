//! stdio 传输 server（RFC 0017 D16 / S2）：与宿主
//! `tiangong_plugin_runtime::sidecar::stdio::StdioSidecarConnection` 对接。
//!
//! 帧协议与 TCP 完全一致；区别仅在通道：stdin 读帧（Auth 首帧校验 token、
//! Request 交业务 dispatch），Response/Notification 写 stdout。stdin EOF
//! 即宿主关闭，进程随之退出——宿主管理生命周期，跳过单例与信号等待。

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tiangong_plugin_runtime::protocol::{
    ErrorCode as PluginErrorCode, IpcFrame, IpcRequest, IpcResponse, Request as PluginRequest,
    Response as PluginResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::singleton::SidecarService;

const TRANSPORT_ENV: &str = "TIANGONG_PLUGIN_TRANSPORT";
const STDIO_TOKEN_ENV: &str = "TIANGONG_PLUGIN_STDIO_TOKEN";
const TRANSPORT_STDIO: &str = "stdio";

/// 宿主是否要求以 stdio 模式运行。
pub fn stdio_requested() -> bool {
    std::env::var(TRANSPORT_ENV).ok().as_deref() == Some(TRANSPORT_STDIO)
}

/// stdio 模式主循环：认证 → 循环分发请求，stdin EOF（宿主关闭）即退出。
pub async fn run_stdio<F>(service_factory: F) -> Result<()>
where
    F: FnOnce() -> Result<Arc<dyn SidecarService>>,
{
    let service_name =
        std::env::var("TIANGONG_PLUGIN_ID").unwrap_or_else(|_| "sidecar".to_string());
    let service_obj = service_factory()?;
    tracing::info!(service = %service_name, "stdio sidecar 开始服务");

    // 通知写出任务：订阅全局广播，与响应共用 stdout 写锁。
    let writer = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    {
        let writer = Arc::clone(&writer);
        let mut notifications = crate::server::notification_broadcast().subscribe();
        tokio::spawn(async move {
            loop {
                match notifications.recv().await {
                    Ok((channel, payload)) => {
                        if write_frame(&writer, &IpcFrame::Notification { channel, payload })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let mut reader = BufReader::new(tokio::io::stdin());
    let mut authenticated = false;
    let expected_token = std::env::var(STDIO_TOKEN_ENV).unwrap_or_default();
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .await
            .context("读取 stdio 帧失败")?;
        if bytes == 0 {
            tracing::info!(service = %service_name, "stdin 已关闭（宿主退出），stdio sidecar 退出");
            break;
        }
        let Ok(frame) = serde_json::from_str::<IpcFrame>(line.trim_end()) else {
            tracing::warn!(service = %service_name, "stdio 收到无法解析的帧");
            continue;
        };
        match frame {
            IpcFrame::Auth(auth) => {
                if auth.token != expected_token {
                    let _ = write_frame(
                        &writer,
                        &IpcFrame::Error {
                            message: "stdio 认证失败：token 不匹配".to_string(),
                        },
                    )
                    .await;
                    bail!("stdio sidecar 认证失败");
                }
                authenticated = true;
            }
            IpcFrame::Request(request) => {
                if !authenticated {
                    let _ = write_frame(
                        &writer,
                        &IpcFrame::Error {
                            message: "stdio 首帧必须是 Auth".to_string(),
                        },
                    )
                    .await;
                    bail!("stdio sidecar 在认证前收到请求");
                }
                if let Err(error) = dispatch_and_respond(&writer, &service_obj, request).await {
                    tracing::warn!(service = %service_name, %error, "stdio 请求处理失败");
                }
            }
            other => {
                tracing::warn!(service = %service_name, frame = ?other, "stdio 收到非预期帧");
            }
        }
    }
    Ok(())
}

async fn dispatch_and_respond(
    writer: &Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    service_obj: &Arc<dyn SidecarService>,
    request: IpcRequest,
) -> Result<()> {
    let plugin_response = match serde_json::from_value::<PluginRequest>(request.payload.clone()) {
        Ok(plugin_request) => service_obj.dispatch(plugin_request).await,
        Err(error) => PluginResponse::error(
            &request.request_id,
            PluginErrorCode::BadRequest,
            format!("解析插件 sidecar 请求失败: {error}"),
            false,
        ),
    };
    let payload =
        serde_json::to_value(&plugin_response).with_context(|| "序列化 sidecar 响应失败")?;
    write_frame(
        writer,
        &IpcFrame::Response(IpcResponse {
            request_id: request.request_id,
            payload,
        }),
    )
    .await
}

async fn write_frame(
    writer: &Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    frame: &IpcFrame,
) -> Result<()> {
    let line = serde_json::to_string(frame).with_context(|| "序列化 stdio 帧失败")?;
    let mut stdout = writer.lock().await;
    stdout
        .write_all(line.as_bytes())
        .await
        .with_context(|| "写入 stdio 帧失败")?;
    stdout
        .write_all(b"\n")
        .await
        .with_context(|| "写入 stdio 换行失败")?;
    stdout.flush().await.with_context(|| "刷新 stdio 帧失败")?;
    Ok(())
}
