//! v2 manifest UI 贡献后端链路验证（T002 解析 + T004 列出 + T006 资源读取）。
//!
//! 用真实 prompt WASM 制品构造 schema v2 插件：manifest 声明
//! `settings.plugin-page` 的 shadow/iframe 两条贡献，验证贡献按 source=manifest
//! 列出、entry HTML 读取、相对资源读取与 `../` 逃逸拒绝。
//!
//! 前置条件：`cargo build -p tiangong-plugin-prompt-wasm --target wasm32-wasip2`。

use std::path::PathBuf;

use tiangong_plugin_runtime::registry::{
    ContributionSource, list_extension_apps, list_slot_contributions, open_manifest_view,
    preload_installed_plugins, read_manifest_resource,
};

fn wasm_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("target/wasm32-wasip2/debug");
    path.push(name);
    path
}

/// 全局插件注册表是进程级单例，本文件用例经此锁串行执行。
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 准备 v2 清单插件：prompt wasm + shadow/iframe 两条设置页贡献 + entry 资源文件。
fn stage_v2_plugin(root: &std::path::Path) {
    let dir = root.join("plugins").join("prompt");
    std::fs::create_dir_all(dir.join("ui")).unwrap();
    std::fs::copy(
        wasm_path("tiangong_plugin_prompt_wasm.wasm"),
        dir.join("tiangong_plugin_prompt_wasm.wasm"),
    )
    .unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        r#"{
            "schema_version": 2,
            "id": "prompt",
            "version": "0.1.0",
            "wasm": { "binary": "tiangong_plugin_prompt_wasm.wasm" },
            "permissions": ["bridge.call"],
            "capabilities": { "tools": false, "prompt": true, "events": ["session.*"] },
            "ui": {
                "contributions": [
                    {
                        "slot": "settings.plugin-page",
                        "id": "prompt-shadow",
                        "title": "自定义指令（Shadow）",
                        "entry": "ui/shadow.html"
                    },
                    {
                        "slot": "settings.plugin-page",
                        "id": "prompt-iframe",
                        "title": "自定义指令（iframe）",
                        "entry": "ui/iframe.html",
                        "sandbox": "iframe"
                    }
                ]
            }
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("ui").join("shadow.html"),
        "<html><body><div id=\"root\"></div><script src=\"app.js\"></script></body></html>",
    )
    .unwrap();
    std::fs::write(
        dir.join("ui").join("iframe.html"),
        "<html><body>iframe page</body></html>",
    )
    .unwrap();
    std::fs::write(
        dir.join("ui").join("app.js"),
        "bridge.call('plugin.ping', '{}');",
    )
    .unwrap();
}

#[test]
fn v2_manifest_贡献经_slot_列出并可读取_entry_与资源() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    let wasm = wasm_path("tiangong_plugin_prompt_wasm.wasm");
    if !wasm.exists() {
        eprintln!(
            "跳过测试：未找到 wasm 组件 {}，请先构建 prompt 插件",
            wasm.display()
        );
        return;
    }

    let root = tempfile::TempDir::new().unwrap();
    stage_v2_plugin(root.path());

    assert_eq!(
        preload_installed_plugins(root.path()),
        1,
        "v2 插件应正常加载"
    );

    // ── 贡献按 Slot 列出，source=manifest，sandbox 归一化正确 ──
    // （注册表为进程级单例，其他用例可能注入别的插件，按 plugin_id 过滤）
    let contributions = list_slot_contributions("settings.plugin-page")
        .into_iter()
        .filter(|item| item.plugin_id == "prompt")
        .collect::<Vec<_>>();
    assert_eq!(contributions.len(), 2, "应列出两条 manifest 贡献");
    assert!(
        contributions
            .iter()
            .all(|item| item.source == ContributionSource::Manifest)
    );

    let shadow = contributions
        .iter()
        .find(|item| item.contribution_id == "prompt-shadow")
        .unwrap();
    // 未声明 sandbox → 归一化为默认 shadow
    assert_eq!(shadow.sandbox, tiangong_plugin_runtime::SandboxKind::Shadow);
    let iframe = contributions
        .iter()
        .find(|item| item.contribution_id == "prompt-iframe")
        .unwrap();
    assert_eq!(iframe.sandbox, tiangong_plugin_runtime::SandboxKind::Iframe);

    // ── entry HTML 读取（子目录 entry）──
    let html = open_manifest_view("prompt", "prompt-shadow").unwrap();
    assert!(html.contains("app.js"));

    // ── 相对资源读取（以 entry 所在目录为根）──
    let (js, mime) = read_manifest_resource("prompt", "prompt-shadow", "app.js").unwrap();
    assert_eq!(mime, "text/javascript");
    assert!(String::from_utf8(js).unwrap().contains("plugin.ping"));

    // ── 路径逃逸拒绝：`../` 到插件目录外 ──
    let escape =
        read_manifest_resource("prompt", "prompt-shadow", "../../../etc/hosts").unwrap_err();
    let message = format!("{escape:#}");
    assert!(
        message.contains("逃出插件目录") || message.contains("路径无效"),
        "逃逸路径应被拒绝: {message}"
    );

    // ── 未知贡献拒绝 ──
    assert!(open_manifest_view("prompt", "nonexistent").is_err());
}

/// 准备声明 extension.tab 的 v2 插件（memory 制品，独立插件 ID 避免与
/// settings 用例的 prompt 注册项在全局注册表冲突）。
fn stage_extension_tab_plugin(root: &std::path::Path, manifest: &str) {
    let dir = root.join("plugins").join("memory");
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::copy(
        wasm_path("tiangong_plugin_memory_wasm.wasm"),
        dir.join("tiangong_plugin_memory_wasm.wasm"),
    )
    .unwrap();
    std::fs::write(dir.join("plugin.json"), manifest).unwrap();
    std::fs::write(
        dir.join("app").join("index.html"),
        "<html><body>board</body></html>",
    )
    .unwrap();
}

#[test]
fn extension_tab_贡献聚合为_app_元数据() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    let wasm = wasm_path("tiangong_plugin_memory_wasm.wasm");
    if !wasm.exists() {
        eprintln!("跳过测试：未找到 wasm 组件 {}", wasm.display());
        return;
    }

    let root = tempfile::TempDir::new().unwrap();
    stage_extension_tab_plugin(
        root.path(),
        r#"{
            "schema_version": 2,
            "id": "memory",
            "version": "0.1.1",
            "wasm": { "binary": "tiangong_plugin_memory_wasm.wasm" },
            "permissions": ["bridge.call"],
            "ui": {
                "contributions": [
                    {
                        "slot": "extension.tab",
                        "id": "board",
                        "title": "看板",
                        "description": "任务看板面板",
                        "icon": "board",
                        "entry": "app/index.html",
                        "open_mode": "multi"
                    },
                    {
                        "slot": "extension.tab",
                        "id": "chart",
                        "title": "图表",
                        "entry": "app/index.html"
                    },
                    {
                        "slot": "settings.plugin-page",
                        "id": "settings",
                        "entry": "app/index.html"
                    }
                ]
            }
        }"#,
    );

    assert_eq!(preload_installed_plugins(root.path()), 1);

    // ── App 列表：仅 extension.tab 贡献，settings 贡献不进入 ──
    let apps = list_extension_apps();
    let memory_apps = apps
        .iter()
        .filter(|app| app.plugin_id == "memory")
        .collect::<Vec<_>>();
    assert_eq!(memory_apps.len(), 2, "两条 extension.tab 贡献聚合为 App");
    let board = memory_apps
        .iter()
        .find(|app| app.contribution_id == "board")
        .unwrap();
    assert_eq!(board.plugin_id, "memory");
    assert_eq!(board.name, "Memory", "插件 descriptor 名称作为 App 名");
    assert_eq!(board.title, "看板");
    assert_eq!(board.description, "任务看板面板");
    assert_eq!(board.open_mode, tiangong_plugin_runtime::OpenMode::Multi);
    assert_eq!(board.sandbox, tiangong_plugin_runtime::SandboxKind::Shadow);

    // open_mode 缺省 singleton
    let chart = memory_apps
        .iter()
        .find(|app| app.contribution_id == "chart")
        .unwrap();
    assert_eq!(
        chart.open_mode,
        tiangong_plugin_runtime::OpenMode::Singleton
    );

    // settings 贡献仍在 Slot 通道可见（不进入 App 列表）
    let memory_settings = list_slot_contributions("settings.plugin-page")
        .into_iter()
        .find(|item| item.plugin_id == "memory")
        .expect("memory 的 settings 贡献应经 Slot 通道可见");
    assert_eq!(memory_settings.source, ContributionSource::Manifest);
}
