//! 通用 sidecar 连接 trait。
//!
//! 由入口侧实现，包装具体的 sidecar 通信（如 MemoryHandle 的 ipc_request）。
//! 注入给 HostState，供 WASM 组件经 `sidecar.invoke` 调用时转发。
//!
//! 通用运行时不理解 sidecar 的业务协议，只做字节透传。

use anyhow::Result;

/// 通用 sidecar 连接：接收原始负载字节，返回原始响应字节。
///
/// 实现方负责：
/// - 连接到正确的 sidecar 进程
/// - 转发请求并等待响应
/// - 处理超时和重连
pub trait SidecarConnection: Send + Sync {
    /// 发送请求负载（JSON 字符串），返回响应负载（JSON 字符串）。
    fn invoke(&self, payload: &str) -> Result<String>;
}
