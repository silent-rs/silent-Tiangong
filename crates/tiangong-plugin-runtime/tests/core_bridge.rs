//! runtime 聚合桥（`RuntimeCorePlugin`）的差量语义验证。
//!
//! Core 只持有桥这一个编译期插件成员；已安装插件的能力经桥聚合可见。
//! 本文件验证：新装插件下一次聚合自然出现、卸载后消失、并发聚合收敛
//! 到同一实例、聚合结果跨次稳定（Core 视角的「稳定插件集合」）。

use std::path::Path;
use std::sync::Arc;

use tiangong_core::tools::extension::{PromptSectionProvider, ToolSpecProvider};
use tiangong_plugin_runtime::RuntimeCorePlugin;

/// 全局插件注册表是进程级单例，本文件用例经此锁串行执行。
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ensure_config() {
    if tiangong_config::registry::try_models().is_none() {
        tiangong_config::registry::init_from_dir(Path::new("/nonexistent"));
    }
}

/// 造一个纯 manifest 的 Desktop TS 工具插件（无 wasm/sidecar）。
/// 校验链：schema v2 + entrypoints 仅 desktop + tool.provide 权限 +
/// capabilities.tools。
fn stage_ts_tool_plugin(root: &Path, id: &str, tool: &str) {
    let dir = root.join("plugins").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        format!(
            r#"{{"schema_version":2,"id":"{id}","version":"0.1.0","entrypoints":["desktop"],"permissions":["tool.provide"],"capabilities":{{"tools":true}},"tools":[{{"name":"{tool}","description":"测试工具","input_schema":{{"type":"object"}}}}]}}"#
        ),
    )
    .unwrap();
}

fn tool_names(plugin: &RuntimeCorePlugin) -> Vec<String> {
    let mut names = plugin
        .tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn 桥聚合_新装插件下一轮可见且卸载后消失() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    ensure_config();
    let root = tempfile::TempDir::new().unwrap();
    stage_ts_tool_plugin(root.path(), "bridge-view-a", "bridge_a_tool");
    assert!(
        tiangong_plugin_runtime::registry::preload_installed_plugins(root.path()) >= 1,
        "插件 A 应装入注册表"
    );

    let bridge = RuntimeCorePlugin::desktop(root.path().to_path_buf());
    assert_eq!(
        tool_names(&bridge),
        vec!["bridge_a_tool"],
        "首轮聚合应见 A 工具"
    );

    // 模拟新装插件 B：registry 出现新记录，下一次聚合即交付（无需通知）。
    stage_ts_tool_plugin(root.path(), "bridge-view-b", "bridge_b_tool");
    tiangong_plugin_runtime::registry::preload_installed_plugins(root.path());
    assert_eq!(
        tool_names(&bridge),
        vec!["bridge_a_tool", "bridge_b_tool"],
        "新装插件 B 下一次聚合可见"
    );

    // prompt 段落聚合为空（两个插件都不声明 prompt），不 panic。
    assert!(bridge.prompt_sections().is_empty());

    // 模拟卸载 B：registry 记录消失，下一次聚合不再交付。
    tiangong_plugin_runtime::registry::uninstall_plugin(root.path(), "bridge-view-b", false)
        .expect("卸载测试插件 B");
    assert_eq!(
        tool_names(&bridge),
        vec!["bridge_a_tool"],
        "卸载后 B 工具消失、其余不受影响"
    );
}

/// 并发聚合首见同一新插件：装载在锁外进行，双方结果一致且线程不卡死。
#[test]
fn 桥聚合_并发首见收敛() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    ensure_config();
    let root = tempfile::TempDir::new().unwrap();
    stage_ts_tool_plugin(root.path(), "bridge-view-c", "bridge_c_tool");
    tiangong_plugin_runtime::registry::preload_installed_plugins(root.path());

    let bridge = Arc::new(RuntimeCorePlugin::desktop(root.path().to_path_buf()));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let bridge = bridge.clone();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let names = tool_names(&bridge);
            tx.send(names).expect("回传聚合结果");
        }));
    }
    drop(tx);
    let first = rx.recv().expect("至少一个结果");
    for names in rx {
        assert_eq!(names, first, "并发聚合结果必须一致");
    }
    for handle in handles {
        handle.join().expect("并发聚合线程不得 panic 或卡死");
    }
    assert_eq!(first, vec!["bridge_c_tool"]);
}
