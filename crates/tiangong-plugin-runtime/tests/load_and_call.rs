//! WASM 插件运行时集成测试。
//!
//! 前置条件：需先构建示例 memory wasm 组件：
//! ```sh
//! cargo build -p tiangong-plugin-memory-wasm --target wasm32-wasip2
//! ```
//! 本测试通过环境变量 `MEMORY_WASM_PATH` 指定 wasm 文件位置，默认指向
//! workspace target 目录下的 debug 产物。

use std::path::PathBuf;
use std::time::Duration;

use tiangong_core::core::Plugin;
use tiangong_core::model::ToolCall as CoreToolCall;
use tiangong_core::session::Session;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_plugin_runtime::{PluginRuntimeConfig, ToolCall, WasmPluginAdapter, WasmPluginLoader};

/// 定位示例 memory wasm 组件文件。
fn memory_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("MEMORY_WASM_PATH") {
        return PathBuf::from(p);
    }
    // 默认：workspace target 目录下 debug 产物。
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // 回到 workspace 根（runtime crate 在 crates/ 下，上溯两级）。
    path.pop();
    path.pop();
    path.push("target/wasm32-wasip2/debug/tiangong_plugin_memory_wasm.wasm");
    path
}

/// 若 wasm 文件不存在则跳过测试（提示先构建组件）。
fn ensure_wasm_or_skip() -> PathBuf {
    let path = memory_wasm_path();
    if !path.exists() {
        eprintln!(
            "跳过测试：未找到 wasm 组件 {}，请先执行 `cargo build -p tiangong-plugin-memory-wasm --target wasm32-wasip2`",
            path.display()
        );
    }
    path
}

#[test]
fn load_and_describe() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let desc = plugin.describe().expect("describe 失败");
    assert_eq!(desc.id, "memory");
    assert_eq!(desc.name, "Memory");
    assert!(!desc.version.is_empty());
}

#[test]
fn tool_specs_contains_recall_memory() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let specs = plugin.tool_specs().expect("tool-specs 失败");
    let recall = specs
        .iter()
        .find(|s| s.name == "recall_memory")
        .expect("应声明 recall_memory 工具");
    assert!(recall.description.contains("回忆"));
    // input_schema 是合法 JSON 文本。
    let schema: serde_json::Value =
        serde_json::from_str(&recall.input_schema).expect("input_schema 应为合法 JSON");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["query"].is_object());
}

#[test]
fn handle_recall_memory_returns_mock_result() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let arguments = r#"{"query":"上次讨论的架构","limit":5}"#.to_string();
    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "call_1".into(),
                name: "recall_memory".into(),
                arguments,
            },
            &config,
        )
        .expect("handle-tool 失败");

    assert!(outcome.ok);
    assert!(
        outcome.summary.contains("上次讨论的架构"),
        "摘要应包含查询词: {}",
        outcome.summary
    );
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn handle_unknown_tool_returns_error() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let result = plugin.handle_tool(
        ToolCall {
            id: "call_2".into(),
            name: "nonexistent_tool".into(),
            arguments: "{}".into(),
        },
        &config,
    );
    assert!(result.is_err(), "未知工具应返回错误");
}

#[test]
fn fuel_limit_traps_runaway_execution() {
    // 给极少 fuel，迫使一次正常调用因 fuel 不足被中断。
    // recall_memory 本身不会死循环，但 1000 fuel 不足以完成调用，从而
    // 证明 fuel 限制机制确实生效：调用会被 trap 终止而非无限执行。
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig {
        fuel_limit: 1_000,
        epoch_deadline: Duration::from_secs(10),
        ..PluginRuntimeConfig::default()
    };
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader
        .load(&wasm, &PluginRuntimeConfig::default())
        .expect("加载 wasm 组件失败");

    let result = plugin.handle_tool(
        ToolCall {
            id: "call_3".into(),
            name: "recall_memory".into(),
            arguments: r#"{"query":"test"}"#.into(),
        },
        &config,
    );
    assert!(
        result.is_err(),
        "极少 fuel 下调用应被中断（实际: {:?})",
        result
    );
}

#[test]
fn adapter_integrates_with_core_plugin_trait() {
    // 验证 WasmPluginAdapter 能被当作进程内 Plugin 使用。
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let adapter = WasmPluginAdapter::new(plugin, config);

    // Plugin::id
    assert_eq!(adapter.id(), "memory");

    // ToolSpecProvider：返回宿主侧 ToolSpec（含反序列化的 input_schema）
    let specs = <WasmPluginAdapter as ToolSpecProvider>::tool_specs(&adapter);
    assert!(specs.iter().any(|s| s.name == "recall_memory"));
    let recall = specs.iter().find(|s| s.name == "recall_memory").unwrap();
    assert!(recall.input_schema.is_object());

    // ToolOverrideHandler：通过 trait 调用 handle（需异步 runtime）
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("创建 tokio runtime 失败");
    let mut session = Session::new("test");
    let call = CoreToolCall {
        id: "call_adapter".into(),
        name: "recall_memory".into(),
        arguments: serde_json::json!({"query": "适配器测试"}),
    };
    let result = rt
        .block_on(<WasmPluginAdapter as ToolOverrideHandler>::handle(
            &adapter,
            &call,
            &mut session,
            "test",
        ))
        .expect("handle 返回 None");
    assert!(result.ok);
    assert!(result.summary.contains("适配器测试"));
}
