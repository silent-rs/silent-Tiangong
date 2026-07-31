//! Memory 插件私有协议。
//!
//! 本 crate 由 Memory WASM 组件和 Memory sidecar 共享，定义两者之间的
//! 请求/响应结构、操作类型和错误码。
//!
//! 约束：
//! - 不依赖 tokio / 数据库 / 网络 / 操作系统 API
//! - 能编译到 wasm32-wasip2
//! - 序列化使用 JSON（载荷在通用 sidecar 接口中以字节传输）

use serde::{Deserialize, Serialize};

/// Memory 私有协议版本。
pub const PROTOCOL_VERSION: &str = "0.1.0";

// ── 请求信封 ──

/// 统一请求信封，WASM → sidecar。
///
/// 避免外层 sidecar.invoke 的 method 和内层 method 重复：
/// 唯一操作来源是 `operation` 字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// 协议版本。
    pub protocol_version: String,
    /// 请求标识（用于匹配响应）。
    pub request_id: String,
    /// 操作类型（唯一来源）。
    pub operation: Operation,
    /// 业务负载（操作特定的 JSON）。
    pub payload: serde_json::Value,
    /// 可选会话 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 可选工作区 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl Request {
    pub fn new(operation: Operation, payload: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: format!("req-{}", simple_id()),
            operation,
            payload,
            session_id: None,
            workspace_id: None,
        }
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_workspace(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }
}

/// Memory 操作类型（唯一来源，避免 method 重复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// 握手（健康检查 + 协议版本校验）。
    Handshake,
    /// 召回上下文（Tool 化回忆）。
    RecallContext,
    /// 粗召回。
    Recall,
    /// 加载三级注入。
    LoadInjection,
    /// 写入事件记忆。
    WriteEpisode,
    /// 更新注入文件。
    UpdateInjection,
    /// 提交记忆候选。
    SubmitCandidate,
    /// 新增/更新手动记忆。
    UpsertManualMemory,
    /// 设置节点状态。
    SetNodeStatus,
    /// 新增/更新关系。
    UpsertRelation,
    /// 删除关系。
    DeleteRelation,
    /// 列出节点。
    ListNodes,
    /// 统计节点。
    CountNodes,
    /// 列出关系。
    ListRelations,
    /// 批量列出关系。
    ListRelationsBatch,
    /// 二跳展开。
    LoadDepth2,
    /// 充分性评估。
    EvaluateRecallSufficiency,
    /// Enhanced Micro 反刍。
    RunEnhancedMicroRumination,
    /// Micro 反刍。
    RunMicroRumination,
    /// Meso 反刍。
    RunMesoRumination,
    /// Meta 反刍。
    RunMetaRumination,
    /// UI：读取配置。
    UiGetConfig,
    /// UI：保存配置。
    UiSetConfig,
    /// UI：通用 Memory 请求（列表/搜索/图谱等）。
    UiMemoryRequest,
    /// 关闭 sidecar。
    Shutdown,
}

// ── 响应信封 ──

/// 统一响应信封，sidecar → WASM。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// 协议版本。
    pub protocol_version: String,
    /// 请求标识（匹配请求）。
    pub request_id: String,
    /// 是否成功。
    pub success: bool,
    /// 响应负载（成功时的业务数据 JSON）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// 业务错误码（失败时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    /// 可读错误信息（失败时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// 是否可重试。
    #[serde(default)]
    pub retryable: bool,
}

impl Response {
    pub fn success(request_id: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.into(),
            success: true,
            payload: Some(payload),
            error_code: None,
            error_message: None,
            retryable: false,
        }
    }

    pub fn error(
        request_id: impl Into<String>,
        code: ErrorCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.into(),
            success: false,
            payload: None,
            error_code: Some(code),
            error_message: Some(message.into()),
            retryable,
        }
    }
}

/// 业务错误码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// sidecar 未配置。
    NotConfigured,
    /// sidecar 尚未启动。
    NotStarted,
    /// sidecar 启动失败。
    StartFailed,
    /// sidecar 当前不可用。
    Unavailable,
    /// 请求超时。
    Timeout,
    /// 请求体过大。
    PayloadTooLarge,
    /// 协议版本不兼容。
    ProtocolMismatch,
    /// 权限不足。
    PermissionDenied,
    /// sidecar 异常退出。
    SidecarCrashed,
    /// Host 内部错误。
    HostError,
    /// 请求解析失败。
    BadRequest,
    /// Memory 被禁用。
    MemoryDisabled,
    /// Memory 业务失败。
    MemoryError,
    /// 模型服务不可用。
    ModelUnavailable,
}

// ── 握手结构 ──

/// 握手请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub protocol_version: String,
}

/// 握手响应（sidecar 返回自身信息）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub plugin_id: String,
    pub plugin_version: String,
    pub sidecar_version: String,
    pub protocol_version: String,
    pub instance_id: String,
    /// 支持的操作列表。
    pub supported_operations: Vec<Operation>,
    /// 服务状态。
    pub status: ServiceStatus,
}

/// sidecar 服务状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    /// 就绪。
    Ready,
    /// 正在初始化。
    Initializing,
    /// 降级（部分能力不可用）。
    Degraded,
}

// ── 序列化辅助 ──

/// 将请求序列化为 JSON 字节（供通用 sidecar 接口传输）。
pub fn encode_request(request: &Request) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(request)
}

/// 从 JSON 字节反序列化请求。
pub fn decode_request(bytes: &[u8]) -> Result<Request, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// 将响应序列化为 JSON 字节。
pub fn encode_response(response: &Response) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(response)
}

/// 从 JSON 字节反序列化响应。
pub fn decode_response(bytes: &[u8]) -> Result<Response, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// 生成简单唯一 ID（不依赖外部库，用计数器 + 时间戳）。
fn simple_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", count, now_millis())
}

/// 获取当前毫秒时间戳（WASM 兼容，用 SystemTime）。
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_roundtrip() {
        let req = Request::new(
            Operation::RecallContext,
            serde_json::json!({"query": "test"}),
        )
        .with_session("sess-1")
        .with_workspace("ws-1");
        let bytes = encode_request(&req).unwrap();
        let decoded = decode_request(&bytes).unwrap();
        assert_eq!(decoded.operation, Operation::RecallContext);
        assert_eq!(decoded.session_id.as_deref(), Some("sess-1"));

        let resp = Response::success(&req.request_id, serde_json::json!({"content": "ok"}));
        let bytes = encode_response(&resp).unwrap();
        let decoded = decode_response(&bytes).unwrap();
        assert!(decoded.success);
        assert_eq!(decoded.request_id, req.request_id);
    }

    #[test]
    fn error_response() {
        let resp = Response::error("req-1", ErrorCode::Timeout, "请求超时", true);
        assert!(!resp.success);
        assert_eq!(resp.error_code, Some(ErrorCode::Timeout));
        assert!(resp.retryable);
    }

    #[test]
    fn handshake() {
        let hs = HandshakeResponse {
            plugin_id: "memory".to_string(),
            plugin_version: "0.1.0".to_string(),
            sidecar_version: "0.1.0".to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            instance_id: "inst-1".to_string(),
            supported_operations: vec![Operation::RecallContext, Operation::WriteEpisode],
            status: ServiceStatus::Ready,
        };
        let json = serde_json::to_string(&hs).unwrap();
        assert!(json.contains("memory"));
        assert!(json.contains("ready"));
    }
}
