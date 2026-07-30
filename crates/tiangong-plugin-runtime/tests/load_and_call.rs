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
use tiangong_plugin_runtime::{
    FusedHit, MemoryKind, PlannedRecall, PluginRuntimeConfig, SearchStrategy, ToolCall,
    WasmPluginAdapter, WasmPluginLoader,
};

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

// ── 阶段二：纯逻辑下沉 + clock host import 测试 ──

/// 加载一个可复用的 WASM 插件实例。
fn load_plugin() -> tiangong_plugin_runtime::WasmPlugin {
    let wasm = ensure_wasm_or_skip();
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::new(&config).expect("创建加载器失败");
    loader.load(&wasm, &config).expect("加载 wasm 组件失败")
}

fn make_hit(node_id: &str, title: &str, score: f64, importance: f64) -> FusedHit {
    FusedHit {
        node_id: node_id.to_string(),
        title: title.to_string(),
        summary: format!("{title} 的摘要内容"),
        score,
        kind: MemoryKind::Episode,
        importance,
        depth1_loaded: false,
    }
}

#[test]
fn rerank_fuse_combines_bm25_and_semantic() {
    let mut plugin = load_plugin();
    // BM25 一路，semantic 一路，二者含一个共同命中（双命中应得奖励）。
    let bm25 = vec![
        make_hit("a", "命中A", 0.9, 0.5),
        make_hit("b", "命中B", 0.5, 0.5),
    ];
    let semantic = vec![
        make_hit("a", "命中A", 0.8, 0.5),
        make_hit("c", "命中C", 0.6, 0.5),
    ];

    let fused = plugin
        .rerank_fuse(bm25, semantic, 0.5, 10)
        .expect("rerank-fuse 失败");

    // 共同命中 A 应因双命中奖励排在最前。
    assert_eq!(fused[0].node_id, "a", "双命中应排在最前");
    assert!(fused.iter().any(|h| h.node_id == "b"));
    assert!(fused.iter().any(|h| h.node_id == "c"));
    // limit 生效。
    assert!(fused.len() <= 3);
}

#[test]
fn rerank_fuse_respects_limit() {
    let mut plugin = load_plugin();
    let bm25 = vec![
        make_hit("a", "A", 0.9, 0.5),
        make_hit("b", "B", 0.8, 0.5),
        make_hit("d", "D", 0.7, 0.5),
    ];
    let fused = plugin
        .rerank_fuse(bm25, vec![], 0.5, 2)
        .expect("rerank-fuse 失败");
    assert_eq!(fused.len(), 2, "应按 limit 截断");
}

#[test]
fn plan_recall_fallback_extracts_history_reference_as_semantic() {
    let mut plugin = load_plugin();
    let planned: PlannedRecall = plugin
        .plan_recall_fallback(
            "继续用刚刚生成的图片".to_string(),
            None,
            vec!["media".to_string()],
            vec![],
            5,
        )
        .expect("plan-recall-fallback 失败");
    assert_eq!(planned.strategy, Some(SearchStrategy::Semantic));
    assert!(planned.keywords.iter().any(|k| k == "media"));
    assert!(!planned.used_llm, "应使用规则路径");
}

#[test]
fn plan_recall_fallback_extracts_file_path_as_keyword() {
    let mut plugin = load_plugin();
    let planned = plugin
        .plan_recall_fallback(
            "查看 /tmp/tiangong/output.png 的历史记录".to_string(),
            None,
            vec![],
            vec![],
            5,
        )
        .expect("plan-recall-fallback 失败");
    assert_eq!(planned.strategy, Some(SearchStrategy::Keyword));
    assert!(planned.keywords.iter().any(|k| k.contains("output.png")));
}

#[test]
fn plan_recall_fallback_skips_plain_chitchat() {
    let mut plugin = load_plugin();
    let planned = plugin
        .plan_recall_fallback("你好".to_string(), None, vec![], vec![], 5)
        .expect("plan-recall-fallback 失败");
    assert_eq!(planned.strategy, Some(SearchStrategy::Skip));
    assert!(planned.query.is_empty());
}

#[test]
fn synthesize_fallback_dedupes_and_formats() {
    let mut plugin = load_plugin();
    let hits = vec![
        make_hit("h1", "历史讨论一", 0.9, 0.8),
        make_hit("h2", "历史讨论二", 0.7, 0.5),
    ];
    let text = plugin
        .synthesize_fallback("架构".to_string(), vec![], hits)
        .expect("synthesize-fallback 失败");
    assert!(text.contains("架构"), "应包含查询词");
    assert!(text.contains("历史讨论一"));
    assert!(text.contains("历史讨论二"));
}

#[test]
fn synthesize_fallback_returns_message_when_no_hits() {
    let mut plugin = load_plugin();
    let text = plugin
        .synthesize_fallback("不存在的内容".to_string(), vec![], vec![])
        .expect("synthesize-fallback 失败");
    assert!(text.contains("未在记忆中找到"));
}

#[test]
fn clock_import_provides_real_time() {
    // recall_memory 的结果摘要内含 host clock 注入的时间戳 t=<ms>。
    // 比较它与宿主当前时间的接近程度，证明 clock host import 生效。
    let mut plugin = load_plugin();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "c1".into(),
                name: "recall_memory".into(),
                arguments: r#"{"query":"时间测试"}"#.into(),
            },
            &PluginRuntimeConfig::default(),
        )
        .expect("handle-tool 失败");
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // 从摘要中提取 t=<ms>。
    let ts = outcome
        .summary
        .split("t=")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse::<u64>().ok())
        .expect("摘要应包含 t=<ms> 时间戳");
    assert!(
        ts >= before && ts <= after,
        "WASM 内时间戳 {ts} 应在宿主调用窗口 [{before}, {after}] 内"
    );
}

#[test]
fn handle_recall_memory_uses_degradation_path() {
    // 阶段二：recall_memory 走 WASM 内降级路径，结果应含规则整理痕迹
    //（不再是阶段一的纯 mock 占位文案）。
    let mut plugin = load_plugin();
    let outcome = plugin
        .handle_tool(
            ToolCall {
                id: "c2".into(),
                name: "recall_memory".into(),
                arguments: r#"{"query":"继续上次的架构设计"}"#.into(),
            },
            &PluginRuntimeConfig::default(),
        )
        .expect("handle-tool 失败");
    assert!(outcome.ok);
    // 降级路径产出应包含「已回忆」整理文案或 Skip 判定，而非阶段一占位文案。
    assert!(
        outcome.summary.contains("已回忆") || outcome.summary.contains("无需历史上下文"),
        "应走降级路径，实际: {}",
        outcome.summary
    );
}
