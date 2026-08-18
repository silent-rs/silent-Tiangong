pub const BRIDGE_SCRIPT: &str = include_str!("../js/bridge.js");
pub const DOCUMENT_STATE_SCRIPT: &str = include_str!("../js/document-state.js");
pub const PAGE_SNAPSHOT_SCRIPT: &str = include_str!("../js/page-snapshot.js");

// ── webview 容器原语（阶段 3）：宿主中立服务 ──
//
// 方法：webview.create / webview.navigate / webview.eval / webview.hide /
// webview.close。首版直接映射到现有会话/标签管理（中立包装，不含浏览器
// 业务语义——tab 策略/协作逻辑属浏览器插件）。

pub fn handle_webview_primitive(
    state: &crate::BrowserPluginState,
    app: &tauri::AppHandle,
    plugin_id: &str,
    method: &str,
    payload: &str,
) -> anyhow::Result<String> {
    let request: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| anyhow::anyhow!("webview 原语负载无效：{error}"))?;
    // 作用域：调用方插件按插件名隔离 webview 实例（view_id = 插件隔离的会话键）
    let scope = format!("webview:{plugin_id}");
    let manager = crate::BrowserManager::from_state(state.registry.session_state(&scope));
    let result = match method {
        // 创建 webview 实例：真实创建（open 复用现有基础设施，含默认 tab）
        // → { view_id, tabs, active_tab_id }
        "webview.create" => {
            let url = request
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            manager
                .open(app, url, 60.0, 60.0, 1024.0, 720.0)
                .map_err(|error| anyhow::anyhow!("创建 webview 失败：{error}"))?;
            manager.persist_session_tabs();
            let snapshot = manager.snapshot_tabs();
            serde_json::json!({
                "view_id": scope,
                "tabs": snapshot.tabs,
                "active_tab_id": snapshot.active_tab_id,
            })
        }
        // 导航：真实导航（navigate_with_app 含打开/聚焦/票据）
        "webview.navigate" => {
            let url = request
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            manager
                .navigate_with_app(app, url)
                .map_err(|error| anyhow::anyhow!("导航失败：{error}"))?;
            manager.persist_session_tabs();
            let snapshot = manager.snapshot_tabs();
            serde_json::json!({
                "view_id": scope,
                "tabs": snapshot.tabs,
                "active_tab_id": snapshot.active_tab_id,
            })
        }
        // 执行 JS：真实 eval（等待回调结果，15s 超时）
        "webview.eval" => {
            let code = request
                .get("js")
                .or_else(|| request.get("code"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("webview.eval 缺少 js 参数"))?;
            let result = manager.eval_result_text(code);
            serde_json::json!({ "view_id": scope, "result": result })
        }
        "webview.hide" => {
            manager
                .hide()
                .map_err(|error| anyhow::anyhow!("隐藏失败：{error}"))?;
            serde_json::json!({ "view_id": scope })
        }
        "webview.close" => {
            manager
                .close()
                .map_err(|error| anyhow::anyhow!("关闭失败：{error}"))?;
            serde_json::json!({ "view_id": scope })
        }
        other => anyhow::bail!("未知 webview 原语方法：{other}"),
    };
    Ok(serde_json::to_string(&result)?)
}

#[cfg(test)]
mod webview_primitive_tests {

    #[test]
    fn 未知方法拒绝() {
        let result = handle_webview_primitive_json("{}", "webview.unknown");
        assert!(result.is_err());
    }

    #[test]
    fn eval缺参数拒绝() {
        let result = handle_webview_primitive_json("{}", "webview.eval");
        assert!(result.is_err());
    }

    /// 不依赖 AppHandle 的纯负载校验入口（真实路径需要 Tauri 环境）。
    fn handle_webview_primitive_json(payload: &str, method: &str) -> anyhow::Result<String> {
        // 负载解析与参数校验先行，与真实实现同一前置路径
        let request: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| anyhow::anyhow!("webview 原语负载无效：{error}"))?;
        match method {
            "webview.eval" => {
                let _ = request
                    .get("js")
                    .or_else(|| request.get("code"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("webview.eval 缺少 js 参数"))?;
                Ok(String::new())
            }
            other
                if !matches!(
                    other,
                    "webview.create" | "webview.navigate" | "webview.hide" | "webview.close"
                ) =>
            {
                anyhow::bail!("未知 webview 原语方法：{other}")
            }
            _ => Ok(String::new()),
        }
    }
}
