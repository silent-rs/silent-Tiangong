//! WASM 插件运行时集成测试。
//!
//! 前置条件：需先构建示例 memory wasm 组件：
//! ```sh
//! cargo build -p tiangong-plugin-memory-wasm --target wasm32-wasip2
//! ```
//! 或 `cargo run -p xtask -- build-wasm`。
//! 本测试默认指向 workspace target 目录下的 debug 产物。

use std::path::PathBuf;
use std::time::Duration;

use tiangong_core::core::Plugin;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::tool_override::ToolSpecProvider;
use tiangong_plugin_runtime::{PluginRuntimeConfig, ToolCall, WasmPluginAdapter, WasmPluginLoader};

/// 定位示例 memory wasm 组件文件。
fn memory_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("MEMORY_WASM_PATH") {
        return PathBuf::from(p);
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("target/wasm32-wasip2/debug/tiangong_plugin_memory_wasm.wasm");
    path
}

/// 若 wasm 文件不存在则提示。
fn ensure_wasm_or_skip() -> PathBuf {
    let path = memory_wasm_path();
    if !path.exists() {
        eprintln!(
            "跳过测试：未找到 wasm 组件 {}，请先执行 `cargo run -p xtask -- build-wasm`",
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
    let schema: serde_json::Value =
        serde_json::from_str(&recall.input_schema).expect("input_schema 应为合法 JSON");
    assert_eq!(schema["type"], "object");
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
    // 给极少 fuel，迫使一次正常调用因 fuel 不足被中断，证明 fuel 限制生效。
    let wasm = ensure_wasm_or_skip();
    let strict = PluginRuntimeConfig {
        fuel_limit: 1_000,
        epoch_deadline: Duration::from_secs(10),
        ..PluginRuntimeConfig::default()
    };
    let loader = WasmPluginLoader::new(&PluginRuntimeConfig::default()).expect("创建加载器失败");
    let mut plugin = loader
        .load(&wasm, &PluginRuntimeConfig::default())
        .expect("加载 wasm 组件失败");

    let result = plugin.handle_tool(
        ToolCall {
            id: "call_3".into(),
            name: "recall_memory".into(),
            arguments: r#"{"query":"test"}"#.into(),
        },
        &strict,
    );
    assert!(result.is_err(), "极少 fuel 下调用应被中断");
}

#[test]
fn handle_recall_memory_without_handle_returns_disabled() {
    // 无 handle 时，经 request 转发应返回 disabled 降级提示（而非错误）。
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "c1".into(),
                name: "recall_memory".into(),
                arguments: r#"{"query":"测试查询"}"#.into(),
            },
            &config,
        )
        .expect("handle-tool 失败");
    assert!(outcome.ok);
    // 无 handle 时应返回降级提示。
    assert!(
        outcome.summary.contains("未启用") || outcome.summary.contains("失败"),
        "无 handle 时应降级提示，实际: {}",
        outcome.summary
    );
}

#[test]
fn handle_recall_memory_with_handle_uses_sidecar() {
    // 注入真实 MemoryHandle 时，经 request 转发到 sidecar，返回真实结果或空。
    let handle = match tiangong_memory::start() {
        Ok(h) => h,
        Err(_) => {
            eprintln!("跳过：MemoryActor 启动失败（可能已有实例运行）");
            return;
        }
    };
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::with_memory(&config, Some(handle)).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "c2".into(),
                name: "recall_memory".into(),
                arguments: r#"{"query":"测试查询"}"#.into(),
            },
            &config,
        )
        .expect("handle-tool 失败");
    assert!(outcome.ok);
    // 有 handle 时应正常返回（不再含"未启用"降级提示）。
    assert!(
        !outcome.summary.contains("未启用"),
        "有 handle 时不应降级，实际: {}",
        outcome.summary
    );
}

#[test]
fn on_config_updated_forwards_to_wasm() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let mut plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");

    let core_config = CoreConfig::default();
    let config_json = serde_json::to_string(&core_config).expect("序列化失败");
    assert!(plugin.on_config_updated(config_json).is_ok());
}

#[test]
fn adapter_integrates_with_core_plugin_trait() {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    let plugin = loader.load(&wasm, &config).expect("加载 wasm 组件失败");
    let adapter = WasmPluginAdapter::new(plugin, config);

    assert_eq!(adapter.id(), "memory");
    let specs = <WasmPluginAdapter as ToolSpecProvider>::tool_specs(&adapter);
    assert!(specs.iter().any(|s| s.name == "recall_memory"));

    // config 事件经 trait 调用不 panic。
    <WasmPluginAdapter as Plugin>::on_config_updated(&adapter, &CoreConfig::default());
}
