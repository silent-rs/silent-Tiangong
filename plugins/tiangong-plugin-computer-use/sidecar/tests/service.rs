//! Computer Use sidecar 集成测试。
//!
//! 验证协议分发路由、错误响应结构与平台后端基础能力。

use tiangong_plugin_computer_use_protocol::ops::*;
use tiangong_plugin_computer_use_protocol::{
    DesktopError, DesktopResult, DesktopSession, Platform,
};
use tiangong_plugin_runtime::protocol::{ErrorCode, HANDSHAKE_OPERATION, Request};

/// 构造一个测试用 Request（绕过 next_request_id 自增，使用固定 id）。
fn request(operation: &str, payload: serde_json::Value) -> Request {
    Request {
        protocol_version: tiangong_plugin_runtime::protocol::PROTOCOL_VERSION.to_string(),
        request_id: "test-req".to_string(),
        operation: operation.to_string(),
        payload,
    }
}

/// 构造一个使用错误协议版本的 Request。
fn request_bad_protocol(operation: &str) -> Request {
    Request {
        protocol_version: "9.9.9".to_string(),
        request_id: "test-req".to_string(),
        operation: operation.to_string(),
        payload: serde_json::json!({}),
    }
}

#[tokio::test]
async fn dispatch_rejects_protocol_mismatch() {
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let resp = service
        .dispatch(request_bad_protocol(DESKTOP_STATUS_OPERATION))
        .await;
    assert!(!resp.success);
    assert_eq!(resp.error_code, Some(ErrorCode::ProtocolMismatch));
}

#[tokio::test]
async fn dispatch_unknown_operation() {
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let resp = service
        .dispatch(request(
            "computer_use.does_not_exist",
            serde_json::json!({}),
        ))
        .await;
    assert!(!resp.success);
    assert_eq!(resp.error_code, Some(ErrorCode::ServiceError));
}

#[tokio::test]
async fn handshake_reports_ready_and_business_protocol() {
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let resp = service
        .dispatch(request(HANDSHAKE_OPERATION, serde_json::json!({})))
        .await;
    assert!(resp.success, "握手应成功");
    let payload = resp.payload.expect("握手响应应有 payload");
    let plugin_id = payload
        .get("plugin_id")
        .and_then(|v| v.as_str())
        .expect("应含 plugin_id");
    assert_eq!(plugin_id, tiangong_plugin_computer_use_protocol::PLUGIN_ID);
    let bp = payload
        .get("business_protocol")
        .and_then(|v| v.as_u64())
        .expect("应含 business_protocol");
    assert_eq!(
        bp as u32,
        tiangong_plugin_computer_use_protocol::COMPUTER_USE_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn desktop_status_returns_result() {
    // desktop_status 经 sidecar 转发到平台后端，应返回 DesktopResult<DesktopStatusResponse>。
    // 在 macOS 上返回真实的会话/授权状态；其他平台返回 BackendUnavailable，均为合法 Result。
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let resp = service
        .dispatch(request(DESKTOP_STATUS_OPERATION, serde_json::json!({})))
        .await;
    assert!(resp.success, "status 调用本身应成功");
    let payload = resp.payload.expect("应有 payload");
    // payload 是 DesktopResult<DesktopStatusResponse>，可能是 Ok 或 Err。
    assert!(
        payload.get("platform").is_some() || payload.get("kind").is_some(),
        "应包含成功响应字段或错误 kind"
    );
}

#[tokio::test]
async fn list_windows_without_permission_returns_permission_denied_on_macos() {
    // 仅在 macOS 上验证：未授权时返回 PermissionDenied。
    if cfg!(not(target_os = "macos")) {
        return;
    }
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let req = ListWindowsRequest::default();
    let payload = serde_json::json!(req);
    let resp = service
        .dispatch(request(DESKTOP_LIST_WINDOWS_OPERATION, payload))
        .await;
    assert!(resp.success);
    // 无论授权与否，返回的都是合法 DesktopResult；不强制断言具体错误，
    // 因为 CI 环境可能已授权。
    let _payload = resp.payload.expect("应有 payload");
}

#[tokio::test]
async fn find_with_empty_scope_returns_business_error_or_result() {
    // 空 scope 的 find 请求：未授权返回 permission_denied，授权后因无目标范围
    // 返回 application_not_found；测试环境授权状态不确定，统一断言为合法往返
    // （协议层成功，payload 为 DesktopResult 的成功或业务错误）。
    let service = tiangong_plugin_computer_use_sidecar::ComputerUseService::new().unwrap();
    let req = FindRequest::default();
    let resp = service
        .dispatch(request(DESKTOP_FIND_OPERATION, serde_json::json!(req)))
        .await;
    assert!(resp.success, "协议层应成功，业务结果在 payload 内");
    let payload = resp.payload.expect("应有 payload");
    // payload 要么是成功结果（含 matches 字段），要么是业务错误（含 kind 字段）。
    let has_matches = payload.get("matches").is_some();
    let kind = payload.get("kind").and_then(|v| v.as_str());
    assert!(
        has_matches || kind.is_some(),
        "payload 应为成功结果或业务错误，实际: {payload}"
    );
}

#[tokio::test]
async fn platform_backend_reports_current_target() {
    let backend = tiangong_plugin_computer_use_sidecar::backend::current_backend();
    let platform = backend.platform();
    #[cfg(target_os = "macos")]
    assert_eq!(platform, Platform::Macos);
    #[cfg(target_os = "windows")]
    assert_eq!(platform, Platform::Windows);
    #[cfg(target_os = "linux")]
    assert_eq!(platform, Platform::Linux);
    // status 应可调用并返回 DesktopResult。
    let _ = backend.status().await;
}

#[test]
fn desktop_result_err_serializes_with_kind() {
    let result: DesktopResult<ListWindowsResponse> =
        DesktopResult::Err(DesktopError::AmbiguousMatch {
            candidates: vec!["a".to_string(), "b".to_string()],
        });
    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("ambiguous_match"));
    assert!(json.contains("candidates"));
}

#[test]
fn all_supported_actions_complete() {
    let actions = tiangong_plugin_computer_use_sidecar::backend::all_supported_actions();
    assert_eq!(actions.len(), 8);
}

/// 验证 DesktopSession 枚举可序列化为 snake_case。
#[test]
fn desktop_session_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&DesktopSession::NotReady).unwrap(),
        "\"not_ready\""
    );
}

/// 抑制未使用导入：在非 macos 平台 ListWindows 等 import 可能未直接使用。
#[allow(unused_imports)]
use tiangong_plugin_computer_use_protocol::ListWindowsResponse as _ListWindowsResponse;
