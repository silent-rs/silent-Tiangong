//! 一次性 command sidecar 路由端到端测试（复刻宿主透明封套序列）：
//! spawn → set_workspace 初始化 → run_command 原样执行 → 响应。
//! 覆盖审查指出的"一次性实例缺少工作区初始化导致命令无法执行"。

use std::path::PathBuf;
use std::time::Duration;

use tiangong_plugin_runtime::sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection};

fn sidecar_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tiangong-command-sidecar"))
}

#[test]
fn ephemeral_route_executes_command_after_workspace_init() {
    let base = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let config = SidecarConfig::new(
        "command",
        "0.0.0",
        sidecar_binary(),
        base.path().join("endpoint.json"),
        base.path().join("sidecar.log"),
        base.path().join("data"),
        std::env::temp_dir(),
    )
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(15));
    let connection = StdioSidecarConnection::new(config);

    // 宿主路由序列第 1 步：初始化会话工作区（缺失时 sidecar 拒绝执行）。
    let init = serde_json::json!({
        "workspace": workspace.path().display().to_string(),
        "full_trust": false,
        "allowed_commands": [],
    });
    connection
        .invoke("command.set_workspace", &init.to_string())
        .expect("set_workspace 初始化失败");

    // 第 2 步：原样执行命令。
    let request = serde_json::json!({
        "cmd": "echo",
        "args": ["ephemeral-e2e-ok"],
        "access": {
            "workspace": workspace.path().display().to_string(),
            "full_trust": false,
            "allowed_commands": [],
        },
        "timeout_secs": 10,
    });
    let raw = connection
        .invoke("command.run_command", &request.to_string())
        .expect("run_command 执行失败");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], true, "命令应成功执行: {raw}");
    assert!(
        response["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("ephemeral-e2e-ok"),
        "输出应包含命令回显: {raw}"
    );

    // 第 3 步：执行后销毁。
    connection.stop().unwrap();
}

#[test]
fn missing_workspace_init_rejects_execution() {
    let base = tempfile::tempdir().unwrap();
    let config = SidecarConfig::new(
        "command",
        "0.0.0",
        sidecar_binary(),
        base.path().join("endpoint.json"),
        base.path().join("sidecar.log"),
        base.path().join("data"),
        std::env::temp_dir(),
    )
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(15));
    let connection = StdioSidecarConnection::new(config);

    // 跳过 set_workspace 直接执行：sidecar 应明确报错（而非静默失败）。
    let request = serde_json::json!({"cmd": "echo", "args": ["x"]});
    let raw = connection
        .invoke("command.run_command", &request.to_string())
        .expect("协议层应返回错误响应");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false);
    assert!(
        response["stderr"]
            .as_str()
            .unwrap_or("")
            .contains("工作目录未注入")
    );
    connection.stop().unwrap();
}
