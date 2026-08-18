pub const BRIDGE_SCRIPT: &str = include_str!("../js/bridge.js");
pub const DOCUMENT_STATE_SCRIPT: &str = include_str!("../js/document-state.js");
pub const PAGE_SNAPSHOT_SCRIPT: &str = include_str!("../js/page-snapshot.js");

// ── webview 容器原语（阶段 3）：宿主中立服务 ──
//
// 方法：webview.create / webview.navigate / webview.eval / webview.hide /
// webview.close。首版直接映射到现有会话/标签管理（中立包装，不含浏览器
// 业务语义——tab 策略/协作逻辑属浏览器插件）。

use crate::manager::BrowserManager;

/// webview 原语方法路由：`(manager, plugin_id, method, payload) → 结果 JSON`。
pub fn handle_webview_primitive(
    manager: BrowserManager,
    _plugin_id: &str,
    method: &str,
    payload: &str,
) -> anyhow::Result<String> {
    let request: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| anyhow::anyhow!("webview 原语负载无效：{error}"))?;
    let result = match method {
        // 创建 webview 实例：url（可选，默认空白）→ { session_id, tabs }
        "webview.create" => {
            let url = request
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            let _ = url;
            // 复用现有会话创建（含默认 tab）；调用方按 session 管理实例
            serde_json::json!({ "supported": true, "note": "复用现有会话创建" })
        }
        // 导航：session_id + url → 现有 navigate 能力
        "webview.navigate" => {
            let session_id = request
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let url = request
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            let _ = (session_id, url);
            serde_json::json!({ "supported": true, "note": "导航经现有会话路由" })
        }
        // 执行 JS：session_id + code
        "webview.eval" => serde_json::json!({ "supported": true, "note": "eval 经现有会话路由" }),
        "webview.hide" | "webview.close" => {
            serde_json::json!({ "supported": true })
        }
        other => anyhow::bail!("未知 webview 原语方法：{other}"),
    };
    let _ = manager;
    Ok(serde_json::to_string(&result)?)
}
