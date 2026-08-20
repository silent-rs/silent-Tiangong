//! 插件自动维护验证：
//! 卸载记录读写往返 → 自动维护计划决策（核心插件自动安装、启用插件自动
//! 升级、黑名单与禁用跳过）→ 卸载写入记录与重新安装清除记录的 registry 集成。

use std::collections::BTreeSet;
use std::path::Path;

use tiangong_plugin_runtime::artifacts::{
    AvailablePlugin, clear_uninstalled_plugin, plan_auto_maintenance, read_uninstalled_plugins,
    record_uninstalled_plugin,
};
use tiangong_plugin_runtime::registry::{install_staged_plugin, uninstall_plugin};

static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn available_plugin(
    id: &str,
    version: &str,
    supported: bool,
    installed: Option<(&str, bool)>,
    update_available: bool,
) -> AvailablePlugin {
    AvailablePlugin {
        id: id.to_string(),
        name: id.to_string(),
        version: version.to_string(),
        description: String::new(),
        supported,
        installed_version: installed.map(|(version, _)| version.to_string()),
        update_available,
        installed_enabled: installed.is_some_and(|(_, enabled)| enabled),
        is_default: false,
        categories: Vec::new(),
    }
}

fn uninstalled_set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

/// 写一个最小合法的纯 UI 清单目录（可直接被卸载或加载）。
fn stage_installed_plugin(root: &Path, id: &str) {
    let dir = root.join("plugins").join(id);
    std::fs::create_dir_all(dir.join("dist")).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        format!(
            r#"{{
                "schema_version": 2,
                "id": "{id}",
                "version": "0.1.0",
                "ui": {{
                    "contributions": [
                        {{
                            "slot": "extension.tab",
                            "id": "{id}",
                            "title": "{id}",
                            "entry": "dist/index.html"
                        }}
                    ]
                }}
            }}"#
        ),
    )
    .unwrap();
    std::fs::write(dir.join("dist").join("index.html"), "<html></html>").unwrap();
}

/// 构造受管事务目录中的待安装插件（纯 UI，无 wasm 无 sidecar）。
fn stage_transaction_plugin(root: &Path, id: &str, version: &str) -> std::path::PathBuf {
    let staging = root
        .join("plugins")
        .join(".transactions")
        .join("staging-test");
    std::fs::create_dir_all(staging.join("dist")).unwrap();
    std::fs::write(
        staging.join("plugin.json"),
        format!(
            r#"{{
                "schema_version": 2,
                "id": "{id}",
                "version": "{version}",
                "ui": {{
                    "contributions": [
                        {{
                            "slot": "extension.tab",
                            "id": "{id}",
                            "title": "{id}",
                            "entry": "dist/index.html"
                        }}
                    ]
                }}
            }}"#
        ),
    )
    .unwrap();
    std::fs::write(staging.join("dist").join("index.html"), "<html></html>").unwrap();
    staging
}

#[test]
fn 卸载记录读写往返与幂等() {
    let root = tempfile::TempDir::new().unwrap();
    assert!(read_uninstalled_plugins(root.path()).is_empty(), "初始为空");

    record_uninstalled_plugin(root.path(), "terminal").unwrap();
    record_uninstalled_plugin(root.path(), "browser").unwrap();
    record_uninstalled_plugin(root.path(), "terminal").unwrap();
    assert_eq!(
        read_uninstalled_plugins(root.path()),
        uninstalled_set(&["terminal", "browser"]),
        "重复记录应幂等"
    );

    clear_uninstalled_plugin(root.path(), "terminal").unwrap();
    clear_uninstalled_plugin(root.path(), "不存在").unwrap();
    assert_eq!(
        read_uninstalled_plugins(root.path()),
        uninstalled_set(&["browser"]),
        "清除后仅剩未清除项"
    );
}

#[test]
fn 损坏的卸载记录按空集合处理() {
    let root = tempfile::TempDir::new().unwrap();
    std::fs::write(root.path().join("uninstalled_plugins.json"), "not-json").unwrap();
    assert!(read_uninstalled_plugins(root.path()).is_empty());

    // 空集合之上继续写入应恢复正常。
    record_uninstalled_plugin(root.path(), "browser").unwrap();
    assert_eq!(
        read_uninstalled_plugins(root.path()),
        uninstalled_set(&["browser"])
    );
}

#[test]
fn 自动维护计划_核心安装_启用升级_黑名单与禁用跳过() {
    let available = vec![
        // 核心插件未安装 → 自动安装。
        available_plugin("terminal", "0.1.0", true, None, false),
        // 核心插件未安装但在卸载黑名单 → 跳过。
        available_plugin("browser", "0.1.0", true, None, false),
        // 核心插件平台不支持 → 跳过。
        available_plugin("interaction", "0.3.1", false, None, false),
        // 核心插件已安装且无更新 → 不动。
        available_plugin("interaction", "0.3.1", true, Some(("0.3.1", true)), false),
        // 已安装、启用、有更新 → 自动升级。
        available_plugin("memory", "0.2.0", true, Some(("0.1.0", true)), true),
        // 已安装、禁用、有更新 → 尊重用户意愿不升级。
        available_plugin("prompt", "0.2.0", true, Some(("0.1.0", false)), true),
        // 已安装、启用、无更新 → 不动。
        available_plugin("fetch", "0.1.0", true, Some(("0.1.0", true)), false),
        // 非核心未安装 → 不自动安装。
        available_plugin("skill", "0.1.0", true, None, false),
        // 已安装、启用、有更新但在黑名单 → 不升级。
        available_plugin("mcp", "0.2.0", true, Some(("0.1.0", true)), true),
    ];
    let uninstalled = uninstalled_set(&["browser", "mcp"]);
    let plan = plan_auto_maintenance(&available, &uninstalled);

    assert_eq!(plan.install, vec!["terminal".to_string()]);
    assert_eq!(plan.upgrade, vec!["memory".to_string()]);
}

#[test]
fn 空目录或全黑名单时计划为空() {
    assert!(
        plan_auto_maintenance(&[], &BTreeSet::new())
            .install
            .is_empty()
    );
    assert!(
        plan_auto_maintenance(&[], &BTreeSet::new())
            .upgrade
            .is_empty()
    );

    let available = vec![available_plugin("terminal", "0.1.0", true, None, false)];
    let uninstalled = uninstalled_set(&["terminal"]);
    let plan = plan_auto_maintenance(&available, &uninstalled);
    assert!(plan.install.is_empty() && plan.upgrade.is_empty());
}

#[test]
fn 卸载写入记录_重新安装清除记录() {
    let _guard = REGISTRY_LOCK.lock().unwrap();
    let root = tempfile::TempDir::new().unwrap();
    tiangong_config::registry::init_from_dir(root.path());
    stage_installed_plugin(root.path(), "terminal");
    assert!(read_uninstalled_plugins(root.path()).is_empty());

    uninstall_plugin(root.path(), "terminal", true).unwrap();
    assert_eq!(
        read_uninstalled_plugins(root.path()),
        uninstalled_set(&["terminal"]),
        "用户主动卸载后应记录"
    );

    // 重新手动安装（走受管事务目录的正式安装链路）成功后记录失效。
    let staging = stage_transaction_plugin(root.path(), "terminal", "0.1.1");
    install_staged_plugin(root.path(), &staging).unwrap();
    assert!(
        read_uninstalled_plugins(root.path()).is_empty(),
        "重新安装成功后卸载记录应清除"
    );
}
