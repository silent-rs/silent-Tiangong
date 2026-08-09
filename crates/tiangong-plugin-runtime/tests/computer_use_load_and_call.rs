//! Computer Use 插件 WASM 运行时集成测试。
//!
//! 前置条件：需先构建 computer-use wasm 组件：
//! ```sh
//! cargo build -p tiangong-plugin-computer-use-wasm --target wasm32-wasip2
//! ```
//! 本测试默认指向 workspace target 目录下的 debug 产物。
//!
//! 覆盖：插件描述符、六个工具声明、工具分发、与 sidecar 的转发链路。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tiangong_plugin_runtime::{PluginRuntimeConfig, SidecarConnection, ToolCall, WasmPluginLoader};

/// 模拟 computer-use sidecar：记录调用并返回构造好的 desktop_status 响应。
#[derive(Default)]
struct MockComputerUseSidecar {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
}

impl MockComputerUseSidecar {
    fn called(&self, operation: &str) -> bool {
        self.calls
            .lock()
            .map(|calls| calls.iter().any(|(item, _)| item == operation))
            .unwrap_or(false)
    }
}

impl SidecarConnection for MockComputerUseSidecar {
    fn invoke(&self, operation: &str, payload: &str) -> anyhow::Result<String> {
        self.invoke_with_progress(operation, payload, &mut |_| {})
    }

    fn invoke_with_progress(
        &self,
        operation: &str,
        payload: &str,
        _on_progress: &mut dyn FnMut(String),
    ) -> anyhow::Result<String> {
        let payload = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("模拟 sidecar 调用记录已损坏"))?
            .push((operation.to_string(), payload));
        // 按操作返回模拟响应（与协议 DesktopResult<T> 对齐）。
        let response = match operation {
            "computer_use.desktop_status" => serde_json::json!({
                "platform": "macos",
                "session": "available",
                "accessibility": { "available": true },
                "supported_actions": ["focus", "press", "set_value"]
            }),
            "computer_use.set_access" => serde_json::json!({}),
            _ => serde_json::json!({ "matches": [], "snapshot": 0, "ambiguous": false }),
        };
        Ok(serde_json::to_string(&response)?)
    }
}

/// 定位 computer-use wasm 组件文件。
fn computer_use_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("COMPUTER_USE_WASM_PATH") {
        return PathBuf::from(p);
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("target/wasm32-wasip2/debug/tiangong_plugin_computer_use_wasm.wasm");
    path
}

/// 若 wasm 文件不存在则提示并跳过当前测试。
fn wasm_or_skip() -> Option<PathBuf> {
    let path = computer_use_wasm_path();
    if !path.exists() {
        eprintln!(
            "跳过测试：未找到 computer-use wasm 组件 {}，请先执行构建",
            path.display()
        );
        return None;
    }
    Some(path)
}

#[test]
fn load_and_describe() {
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let desc = plugin.describe().expect("describe 失败");
    assert_eq!(desc.id, "computer-use");
    assert_eq!(desc.name, "Computer Use");
    assert!(!desc.version.is_empty());
}

#[test]
fn tool_specs_contains_six_tools() {
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let specs = plugin.tool_specs().expect("tool-specs 失败");
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"desktop_status"), "应声明 desktop_status");
    assert!(
        names.contains(&"desktop_list_windows"),
        "应声明 desktop_list_windows"
    );
    assert!(
        names.contains(&"desktop_snapshot"),
        "应声明 desktop_snapshot"
    );
    assert!(names.contains(&"desktop_find"), "应声明 desktop_find");
    assert!(names.contains(&"desktop_action"), "应声明 desktop_action");
    assert!(names.contains(&"desktop_wait"), "应声明 desktop_wait");
    assert_eq!(names.len(), 6, "应恰好声明六个工具");

    // 验证 desktop_action 的 schema 包含 element/action 必填字段。
    let action = specs
        .iter()
        .find(|s| s.name == "desktop_action")
        .expect("应声明 desktop_action");
    let schema: serde_json::Value =
        serde_json::from_str(&action.input_schema).expect("input_schema 应为合法 JSON");
    assert_eq!(schema["type"], "object");
    assert!(
        schema["required"].as_array().is_some(),
        "desktop_action schema 应声明 required 字段"
    );
}

#[test]
fn handle_unknown_tool_returns_error() {
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let result = plugin.handle_tool(
        ToolCall {
            id: "call_1".into(),
            name: "nonexistent_tool".into(),
            arguments: "{}".into(),
        },
        &config,
    );
    assert!(result.is_err(), "未知工具应返回错误");
}

#[test]
fn handle_desktop_status_without_sidecar_returns_error() {
    // 无 sidecar 时，desktop_status 调用转发失败应返回错误（而非 panic）。
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let result = plugin.handle_tool(
        ToolCall {
            id: "call_2".into(),
            name: "desktop_status".into(),
            arguments: "{}".into(),
        },
        &config,
    );
    assert!(result.is_err(), "无 sidecar 时 desktop_status 应返回错误");
}

#[test]
fn handle_desktop_status_with_mock_sidecar_returns_status() {
    // 有 mock sidecar 时，desktop_status 经 wasm 转发并返回平台状态。
    let sidecar = Arc::new(MockComputerUseSidecar::default());
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader =
        WasmPluginLoader::with_sidecar(&config, Some(sidecar.clone())).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "call_3".into(),
                name: "desktop_status".into(),
                arguments: "{}".into(),
            },
            &config,
        )
        .expect("handle-tool 失败");
    assert!(outcome.ok, "desktop_status 应成功");
    assert!(sidecar.called("computer_use.desktop_status"));
    // stdout 应含序列化的 DesktopStatusResponse。
    let stdout: serde_json::Value =
        serde_json::from_str(&outcome.stdout).expect("stdout 应为合法 JSON");
    assert_eq!(stdout["platform"], "macos");
    assert_eq!(stdout["session"], "available");
}

#[test]
fn handle_desktop_action_missing_element_returns_failure() {
    // desktop_action 缺少 element 参数时应返回失败 ToolResult（而非错误）。
    let sidecar = Arc::new(MockComputerUseSidecar::default());
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader =
        WasmPluginLoader::with_sidecar(&config, Some(sidecar.clone())).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "call_4".into(),
                name: "desktop_action".into(),
                arguments: r#"{"action":"press"}"#.into(),
            },
            &config,
        )
        .expect("handle-tool 失败");
    assert!(!outcome.ok, "缺少 element 时应为失败");
    assert!(
        outcome.stderr.contains("element"),
        "stderr 应提示缺少 element"
    );
}

#[test]
fn prompt_sections_injects_usage_guidance() {
    // prompt_sections 应返回桌面控制使用指导段落。
    let Some(wasm) = wasm_or_skip() else {
        return;
    };
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");
    let adapter = tiangong_plugin_runtime::WasmPluginAdapter::new(plugin, config);

    use tiangong_core::tool_override::PromptSectionProvider;
    let sections =
        <tiangong_plugin_runtime::WasmPluginAdapter as PromptSectionProvider>::prompt_sections(
            &adapter,
        );
    assert!(
        sections
            .iter()
            .any(|s| s.contains("computer-use") || s.contains("desktop")),
        "prompt 应包含桌面控制使用指导"
    );
}
