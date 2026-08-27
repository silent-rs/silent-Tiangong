//! command 一次性沙箱路由的真实执行验证。
//!
//! 真实 Launcher 用例依赖已构建的 `tiangong-sandbox` 与 command sidecar；
//! 平台不支持嵌套沙箱时明确跳过。真实 Launcher 与无沙箱 stdio 链路分别
//! 验证取消和宿主停止后的后代进程清理。

#[cfg(any(unix, windows))]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use tiangong_plugin_command_protocol::{COMMAND_PROTOCOL_VERSION, PLUGIN_VERSION};
use tiangong_plugin_runtime::manifest::SidecarLifecycle;
use tiangong_plugin_runtime::protocol::PROTOCOL_VERSION;
#[cfg(any(unix, windows))]
use tiangong_plugin_runtime::sidecar::SidecarInvocationContext;
use tiangong_plugin_runtime::sidecar::{
    EphemeralCommandConnection, SidecarConfig, SidecarConnection, StdioSidecarConnection,
};

const RUN_SHELL_OPERATION: &str = "command.run_shell";
#[cfg(windows)]
const RUN_COMMAND_OPERATION: &str = "command.run_command";
const SET_WORKSPACE_OPERATION: &str = "command.set_workspace";
#[cfg(windows)]
const WINDOWS_PROCESS_TREE_MARKER: &str = ".tiangong-process-tree-helper";
#[cfg(windows)]
const WINDOWS_SANDBOX_PROBE_REQUEST: &str = ".tiangong-windows-sandbox-probe.json";
#[cfg(windows)]
const WINDOWS_CONCURRENT_MARKER: &str = ".tiangong-windows-concurrent-helper";

#[cfg(any(unix, windows))]
static REAL_SANDBOX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct WindowsSandboxProbeRequest {
    workspace: PathBuf,
    existing_workspace_file: PathBuf,
    outside_write: PathBuf,
    outside_read: PathBuf,
    outside_delete: PathBuf,
    outside_config: PathBuf,
    git_config: PathBuf,
    sensitive_paths: Vec<PathBuf>,
    traversal_target: PathBuf,
    report_path: PathBuf,
    child_report_path: PathBuf,
    network_address: String,
    resource_limits: tiangong_sandbox::SandboxResourceLimits,
}

#[cfg(windows)]
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WindowsSandboxProbeReport {
    workspace_write: bool,
    existing_workspace_read_write: bool,
    dedicated_temp_write: bool,
    outside_write_blocked: bool,
    outside_read_blocked: bool,
    outside_delete_blocked: bool,
    outside_config_write_blocked: bool,
    git_metadata_write_blocked: bool,
    git_metadata_readable: bool,
    network_blocked: bool,
    sensitive_read_blocked: bool,
    path_traversal_blocked: bool,
    policy_envelope_hidden: bool,
    resource_limits_applied: bool,
    child_restrictions_inherited: bool,
}

fn debug_binary(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let debug_dir = executable.parent()?.parent()?;
    let candidate = debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn command_sidecar_binary() -> Option<PathBuf> {
    debug_binary("tiangong-command-sidecar")
}

#[cfg(any(unix, windows))]
fn sandbox_binaries_ready() -> bool {
    if !matches!(
        tiangong_sandbox::availability(),
        tiangong_sandbox::SandboxAvailability::Available
    ) {
        eprintln!("跳过真实 Launcher 测试：当前环境不支持嵌套操作系统沙箱");
        return false;
    }
    if debug_binary("tiangong-sandbox").is_none() {
        eprintln!("跳过真实 Launcher 测试：target/debug/tiangong-sandbox 尚未构建");
        return false;
    }
    if command_sidecar_binary().is_none() {
        eprintln!("跳过真实 Launcher 测试：target/debug/tiangong-command-sidecar 尚未构建");
        return false;
    }
    true
}

#[cfg(any(unix, windows))]
struct SandboxFixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    fake_home: PathBuf,
    trust_db: PathBuf,
    sidecar_log: PathBuf,
    connection: Arc<EphemeralCommandConnection>,
}

#[cfg(any(unix, windows))]
fn sandbox_fixture() -> Option<SandboxFixture> {
    if !sandbox_binaries_ready() {
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let storage_root = root.path().join("storage");
    let plugin_root = storage_root.join("plugins/command");
    let workspace = storage_root.join("workspaces/session-a");
    let outside = root.path().join("outside/blocked.txt");
    let fake_home = root.path().join("home");
    let ssh_dir = fake_home.join(".ssh");
    let aws_dir = fake_home.join(".aws");
    let trust_db = storage_root.join("trust.db");
    std::fs::create_dir_all(&plugin_root).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&ssh_dir).unwrap();
    std::fs::create_dir_all(&aws_dir).unwrap();
    std::fs::write(ssh_dir.join("id_ed25519"), "TIANGONG_FAKE_SSH_SECRET").unwrap();
    std::fs::write(aws_dir.join("credentials"), "TIANGONG_FAKE_AWS_SECRET").unwrap();
    std::fs::write(&trust_db, "TIANGONG_FAKE_TRUST_SECRET").unwrap();

    let binary_name = format!("tiangong-command-sidecar{}", std::env::consts::EXE_SUFFIX);
    let binary = plugin_root.join(&binary_name);
    std::fs::copy(command_sidecar_binary().unwrap(), &binary).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        plugin_root.join("plugin.json"),
        serde_json::json!({
            "id": "command",
            "sidecar": { "binary": "tiangong-command-sidecar" }
        })
        .to_string(),
    )
    .unwrap();

    let sidecar_log = plugin_root.join("unused-sidecar.log");
    let config = SidecarConfig::new(
        "command",
        PLUGIN_VERSION,
        binary,
        plugin_root.join("unused-endpoint.json"),
        sidecar_log.clone(),
        plugin_root.join("unused-data"),
        storage_root,
    )
    .with_protocols(PROTOCOL_VERSION, COMMAND_PROTOCOL_VERSION)
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(90))
    .with_sandbox_program_root(Some(plugin_root))
    .with_sandbox_denied_read_paths(vec![ssh_dir, aws_dir, trust_db.clone()]);
    let connection = Arc::new(EphemeralCommandConnection::new(config));
    connection.update_exec_env(BTreeMap::from([(
        "HOME".to_string(),
        fake_home.display().to_string(),
    )]));

    Some(SandboxFixture {
        _root: root,
        workspace,
        outside,
        fake_home,
        trust_db,
        sidecar_log,
        connection,
    })
}

#[cfg(any(unix, windows))]
fn invocation_context(workspace: &Path, invocation_id: &str) -> SidecarInvocationContext {
    SidecarInvocationContext {
        session_id: "session-a".to_string(),
        invocation_id: invocation_id.to_string(),
        authoritative_workspace: workspace.to_path_buf(),
    }
}

#[cfg(windows)]
fn windows_command_request(
    executable: &Path,
    test_name: &str,
    workspace: &Path,
    timeout_secs: u64,
) -> String {
    let executable = executable.display().to_string().replace('\\', "/");
    serde_json::json!({
        "cmd": format!("\"{executable}\""),
        "args": [test_name, "--exact", "--nocapture"],
        "cwd": workspace.display().to_string(),
        "timeout_secs": timeout_secs,
        "workspace": workspace.display().to_string(),
        "full_trust": true,
        "allowed_commands": [],
    })
    .to_string()
}

#[cfg(windows)]
fn copy_current_test_binary(workspace: &Path, name: &str) -> PathBuf {
    let source = std::env::current_exe().expect("读取 Windows 测试程序路径失败");
    let target = workspace.join(format!("{name}.exe"));
    std::fs::copy(source, &target).expect("复制 Windows Sandbox 测试程序失败");
    target
}

#[cfg(windows)]
fn reachable_windows_network_address() -> String {
    use std::net::ToSocketAddrs;

    for endpoint in ["github.com:443", "www.microsoft.com:443", "1.1.1.1:443"] {
        let Ok(addresses) = endpoint.to_socket_addrs() else {
            continue;
        };
        for address in addresses {
            if std::net::TcpStream::connect_timeout(&address, Duration::from_secs(3)).is_ok() {
                return address.to_string();
            }
        }
    }
    panic!("Windows 测试环境没有可达的网络目标");
}

#[cfg(unix)]
fn shell_request(script: String, claimed_workspace: &Path) -> String {
    shell_request_with_timeout(script, claimed_workspace, 15)
}

#[cfg(unix)]
fn shell_request_with_timeout(
    script: String,
    claimed_workspace: &Path,
    timeout_secs: u64,
) -> String {
    serde_json::json!({
        "script": script,
        "shell": "sh",
        "timeout_secs": timeout_secs,
        "workspace": claimed_workspace.display().to_string(),
        "full_trust": true,
        "allowed_commands": [],
    })
    .to_string()
}

#[cfg(unix)]
fn invoke_shell(fixture: &SandboxFixture, id: &str, script: String) -> serde_json::Value {
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &shell_request(script, &fixture.workspace),
            &invocation_context(&fixture.workspace, id),
        )
        .unwrap_or_else(|error| {
            panic!(
                "攻击验证 {id} 调用失败: {error:#}；sidecar 日志:\n{}",
                read_log(&fixture.sidecar_log)
            )
        });
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("攻击验证响应无效: {error}: {raw}"))
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(target_os = "linux")]
fn concurrent_process_snapshot() -> String {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-eo", "pid=,ppid=,pgid=,stat=,wchan:32=,args="])
        .output()
    else {
        return "<无法执行 ps>".to_string();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            [
                "tiangong-sandbox",
                "tiangong-command-sidecar",
                "bwrap",
                "ephemeral_route",
            ]
            .iter()
            .any(|needle| line.contains(needle))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(target_os = "linux")]
fn concurrent_sidecar_logs() -> String {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return "<无法扫描系统临时目录>".to_string();
    };
    let mut logs = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("tiangong-command-")
        })
        .map(|entry| entry.path().join("tmp/sidecar.log"))
        .filter(|path| path.is_file())
        .map(|path| format!("{}:\n{}", path.display(), read_log(&path)))
        .collect::<Vec<_>>();
    logs.sort();
    if logs.is_empty() {
        "<未发现一次性 sidecar 日志>".to_string()
    } else {
        logs.join("\n\n")
    }
}

#[cfg(target_os = "linux")]
fn invocation_diagnostics(fallback_log: &Path) -> String {
    format!(
        "模板日志:\n{}\n\n活动进程:\n{}\n\n一次性 sidecar 日志:\n{}",
        read_log(fallback_log),
        concurrent_process_snapshot(),
        concurrent_sidecar_logs()
    )
}

#[cfg(not(target_os = "linux"))]
fn invocation_diagnostics(fallback_log: &Path) -> String {
    read_log(fallback_log)
}

#[cfg(unix)]
#[test]
fn real_launcher_enforces_workspace_and_dedicated_temp() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };

    let inside_request = shell_request(
        "test -n \"$TMPDIR\" && /usr/bin/touch \"$TMPDIR/probe\" && /usr/bin/touch inside.txt && printf '%s' \"$TMPDIR\""
            .to_string(),
        fixture.outside.parent().unwrap(),
    );
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &inside_request,
            &invocation_context(&fixture.workspace, "inside-and-temp"),
        )
        .expect("真实 Launcher 工作区内命令执行失败");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], true, "工作区与临时目录应可写: {raw}");
    assert!(fixture.workspace.join("inside.txt").is_file());
    assert!(
        response["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("tiangong-command-")
    );

    let outside_request = shell_request(
        format!("/usr/bin/touch {}", shell_quote(&fixture.outside)),
        &fixture.workspace,
    );
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &outside_request,
            &invocation_context(&fixture.workspace, "outside"),
        )
        .expect("真实 Launcher 应返回 command 失败响应");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false, "工作区外写入必须失败: {raw}");
    assert!(!fixture.outside.exists());
}

#[cfg(windows)]
#[test]
fn real_launcher_enforces_windows_command_sandbox() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };

    let outside_root = fixture.outside.parent().unwrap();
    let outside_read = outside_root.join("read-secret.txt");
    let outside_delete = outside_root.join("delete-root");
    let outside_config = fixture.fake_home.join("profile.ini");
    let git_dir = fixture.workspace.join(".git");
    let git_config = git_dir.join("config");
    let existing_workspace_file = fixture.workspace.join("existing.txt");
    std::fs::create_dir_all(&outside_delete).unwrap();
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(&outside_read, "TIANGONG_FAKE_OUTSIDE_SECRET").unwrap();
    std::fs::write(outside_delete.join("keep"), "safe").unwrap();
    std::fs::write(&outside_config, "safe\n").unwrap();
    std::fs::write(&git_config, "safe\n").unwrap();
    std::fs::write(&existing_workspace_file, "workspace-before").unwrap();

    let probe_binary = copy_current_test_binary(&fixture.workspace, "sandbox-probe");
    let report_path = fixture.workspace.join("sandbox-probe-report.json");
    let child_report_path = fixture.workspace.join("sandbox-child-report.json");
    let request = WindowsSandboxProbeRequest {
        workspace: fixture.workspace.clone(),
        existing_workspace_file: existing_workspace_file.clone(),
        outside_write: fixture.outside.clone(),
        outside_read,
        outside_delete: outside_delete.clone(),
        outside_config: outside_config.clone(),
        git_config: git_config.clone(),
        sensitive_paths: vec![
            fixture.fake_home.join(".ssh/id_ed25519"),
            fixture.fake_home.join(".aws/credentials"),
            fixture.trust_db.clone(),
        ],
        traversal_target: fixture.workspace.join("../../../outside/traversal.txt"),
        report_path: report_path.clone(),
        child_report_path,
        network_address: reachable_windows_network_address(),
        resource_limits: tiangong_sandbox::SandboxResourceLimits::default(),
    };
    std::fs::write(
        fixture.workspace.join(WINDOWS_SANDBOX_PROBE_REQUEST),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();

    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_COMMAND_OPERATION,
            &windows_command_request(
                &probe_binary,
                "windows_command_sandbox_probe_helper",
                &fixture.workspace,
                30,
            ),
            &invocation_context(&fixture.workspace, "windows-enforcement"),
        )
        .unwrap_or_else(|error| {
            panic!(
                "Windows 真实 Launcher 调用失败: {error:#}；sidecar 日志:\n{}",
                read_log(&fixture.sidecar_log)
            )
        });
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], true, "Windows 隔离探针执行失败: {raw}");
    let report: WindowsSandboxProbeReport =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(report.workspace_write, "工作区写入未放行: {report:?}");
    assert!(
        report.existing_workspace_read_write,
        "工作区已有文件不可读写: {report:?}"
    );
    assert!(
        report.dedicated_temp_write,
        "专用临时目录不可写: {report:?}"
    );
    assert!(
        report.outside_write_blocked,
        "工作区外写入未阻断: {report:?}"
    );
    assert!(
        report.outside_read_blocked,
        "工作区外读取未阻断: {report:?}"
    );
    assert!(
        report.outside_delete_blocked,
        "工作区外删除未阻断: {report:?}"
    );
    assert!(
        report.outside_config_write_blocked,
        "用户配置写入未阻断: {report:?}"
    );
    assert!(
        report.git_metadata_write_blocked,
        "Git 元数据写入未阻断: {report:?}"
    );
    assert!(report.git_metadata_readable, "Git 元数据不可读: {report:?}");
    assert!(report.network_blocked, "command 网络未阻断: {report:?}");
    assert!(
        report.sensitive_read_blocked,
        "用户凭据读取未阻断: {report:?}"
    );
    assert!(
        report.path_traversal_blocked,
        "路径穿越写入未阻断: {report:?}"
    );
    assert!(
        report.policy_envelope_hidden,
        "Launcher 内部策略泄漏给 command: {report:?}"
    );
    assert!(
        report.resource_limits_applied,
        "Windows Job 资源限制未生效: {report:?}"
    );
    assert!(
        report.child_restrictions_inherited,
        "command 子进程未继承隔离: {report:?}"
    );
    assert!(!fixture.outside.exists());
    assert!(outside_delete.join("keep").is_file());
    assert_eq!(std::fs::read_to_string(outside_config).unwrap(), "safe\n");
    assert_eq!(std::fs::read_to_string(git_config).unwrap(), "safe\n");
    assert_eq!(
        std::fs::read_to_string(existing_workspace_file).unwrap(),
        "workspace-after"
    );
}

#[cfg(unix)]
#[test]
fn real_launcher_blocks_dangerous_attack_matrix() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };

    for (id, path, marker) in [
        (
            "read_ssh",
            fixture.fake_home.join(".ssh/id_ed25519"),
            "TIANGONG_FAKE_SSH_SECRET",
        ),
        (
            "read_aws",
            fixture.fake_home.join(".aws/credentials"),
            "TIANGONG_FAKE_AWS_SECRET",
        ),
        (
            "read_trust",
            fixture.trust_db.clone(),
            "TIANGONG_FAKE_TRUST_SECRET",
        ),
    ] {
        let response = invoke_shell(&fixture, id, format!("/bin/cat {}", shell_quote(&path)));
        assert!(
            !response["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains(marker),
            "敏感内容泄漏: {id}: {response}"
        );
        eprintln!("SAFE: {id}");
    }

    let delete_root = fixture.outside.parent().unwrap().join("delete-root");
    std::fs::create_dir_all(&delete_root).unwrap();
    std::fs::write(delete_root.join("keep"), "safe").unwrap();
    let response = invoke_shell(
        &fixture,
        "delete_outside",
        format!("/bin/rm -rf {}", shell_quote(&delete_root)),
    );
    assert_eq!(response["ok"], false, "工作区外删除应失败: {response}");
    assert!(delete_root.join("keep").is_file());
    eprintln!("SAFE: delete_outside");

    let hosts = fixture.outside.parent().unwrap().join("etc/hosts");
    std::fs::create_dir_all(hosts.parent().unwrap()).unwrap();
    std::fs::write(&hosts, "safe\n").unwrap();
    let response = invoke_shell(
        &fixture,
        "write_system_config",
        format!("printf PWNED >> {}", shell_quote(&hosts)),
    );
    assert_eq!(response["ok"], false, "系统配置写入应失败: {response}");
    assert_eq!(std::fs::read_to_string(&hosts).unwrap(), "safe\n");
    eprintln!("SAFE: write_system_config");

    let bashrc = fixture.fake_home.join(".bashrc");
    std::fs::write(&bashrc, "safe\n").unwrap();
    let response = invoke_shell(
        &fixture,
        "write_bashrc",
        format!("printf PWNED >> {}", shell_quote(&bashrc)),
    );
    assert_eq!(response["ok"], false, "用户配置写入应失败: {response}");
    assert_eq!(std::fs::read_to_string(&bashrc).unwrap(), "safe\n");
    eprintln!("SAFE: write_bashrc");

    let git_dir = fixture.workspace.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let git_config = git_dir.join("config");
    std::fs::write(&git_config, "safe\n").unwrap();
    let response = invoke_shell(
        &fixture,
        "git_history",
        format!("printf PWNED > {}", shell_quote(&git_config)),
    );
    assert_eq!(response["ok"], false, "git 元数据写入应失败: {response}");
    assert_eq!(std::fs::read_to_string(&git_config).unwrap(), "safe\n");
    eprintln!("SAFE: git_history");

    let link_target = fixture.outside.parent().unwrap().join("link-target");
    std::fs::write(&link_target, "safe\n").unwrap();
    let link = fixture.workspace.join("escape-link");
    std::os::unix::fs::symlink(&link_target, &link).unwrap();
    let response = invoke_shell(
        &fixture,
        "symlink_escape",
        format!("printf PWNED > {}", shell_quote(&link)),
    );
    assert_eq!(response["ok"], false, "符号链接逃逸应失败: {response}");
    assert_eq!(std::fs::read_to_string(&link_target).unwrap(), "safe\n");
    eprintln!("SAFE: symlink_escape");
}

#[cfg(unix)]
#[test]
fn real_launcher_sanitizes_environment() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    fixture.connection.update_exec_env(BTreeMap::from([
        ("HOME".into(), fixture.fake_home.display().to_string()),
        ("LD_PRELOAD".into(), "PWNED_RUNTIME_LD".into()),
        ("LD_LIBRARY_PATH".into(), "PWNED_RUNTIME_LIBRARY".into()),
        ("DYLD_INSERT_LIBRARIES".into(), "PWNED_RUNTIME_DYLD".into()),
        ("BASH_ENV".into(), "PWNED_RUNTIME_BASH".into()),
    ]));
    std::fs::write(
        fixture.workspace.join(".env"),
        "LD_PRELOAD=PWNED_FILE_LD\nDYLD_INSERT_LIBRARIES=PWNED_FILE_DYLD\nBASH_ENV=PWNED_FILE_BASH\nPATH=PWNED_FILE_PATH\nTMPDIR=PWNED_FILE_TMP\nTMP=PWNED_FILE_TMP\nTEMP=PWNED_FILE_TMP\n",
    )
    .unwrap();

    let response = invoke_shell(&fixture, "sanitized-env", "/usr/bin/env".to_string());
    assert_eq!(response["ok"], true, "环境检查命令应成功: {response}");
    let stdout = response["stdout"].as_str().unwrap_or_default();
    assert!(!stdout.contains("PWNED_"), "危险环境变量泄漏: {stdout}");
    for key in ["TMPDIR", "TMP", "TEMP"] {
        let value = stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("缺少 {key}: {stdout}"));
        assert!(value.contains("tiangong-command-"), "{key} 未使用专用目录");
    }
}

#[cfg(unix)]
#[test]
fn real_launcher_cancel_kills_background_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let pid_file = fixture.workspace.join("background.pid");
    let heartbeat_file = fixture.workspace.join("background.heartbeat");
    let request = shell_request(
        unix_background_process_script(&pid_file, &heartbeat_file),
        &fixture.workspace,
    );
    let worker_connection = Arc::clone(&fixture.connection);
    let context = invocation_context(&fixture.workspace, "cancel-background");
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = worker_connection.invoke_with_context(RUN_SHELL_OPERATION, &request, &context);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(
        &pid_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    wait_for_heartbeat_growth(
        &heartbeat_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    #[cfg(not(target_os = "linux"))]
    assert!(process_exists(background_pid));
    #[cfg(target_os = "linux")]
    let _ = background_pid;
    fixture.connection.cancel_session("session-a").unwrap();
    let _ = finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("取消会话后 command 执行线程未及时结束");
    worker.join().unwrap();
    #[cfg(target_os = "linux")]
    wait_for_heartbeat_stop(&heartbeat_file, Duration::from_secs(5));
    #[cfg(not(target_os = "linux"))]
    wait_for_process_exit(background_pid, Duration::from_secs(5));
    eprintln!("SAFE: background_persistence");
}

#[cfg(unix)]
#[test]
fn real_launcher_timeout_kills_background_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let pid_file = fixture.workspace.join("timeout-background.pid");
    let heartbeat_file = fixture.workspace.join("timeout-background.heartbeat");
    let request = shell_request_with_timeout(
        unix_background_process_script(&pid_file, &heartbeat_file),
        &fixture.workspace,
        1,
    );
    let worker_connection = Arc::clone(&fixture.connection);
    let context = invocation_context(&fixture.workspace, "timeout-background");
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = worker_connection.invoke_with_context(RUN_SHELL_OPERATION, &request, &context);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(
        &pid_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    wait_for_heartbeat_growth(
        &heartbeat_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    let raw = finished_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("命令超时后调用线程未及时结束")
        .expect("命令超时应返回结构化响应");
    worker.join().unwrap();
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false, "超时命令不应成功: {raw}");
    assert!(
        response["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("超时"),
        "超时响应应明确说明原因: {raw}"
    );
    wait_for_heartbeat_stop(&heartbeat_file, Duration::from_secs(5));
    #[cfg(not(target_os = "linux"))]
    wait_for_process_exit(background_pid, Duration::from_secs(5));
    #[cfg(target_os = "linux")]
    let _ = background_pid;
    eprintln!("SAFE: timeout_process_tree");
}

#[cfg(windows)]
#[test]
fn real_launcher_cancel_kills_windows_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let helper_binary = copy_current_test_binary(&fixture.workspace, "cancel-tree-helper");
    std::fs::write(fixture.workspace.join(WINDOWS_PROCESS_TREE_MARKER), "ready").unwrap();
    let pid_file = fixture.workspace.join("background.pid");
    let request = windows_background_process_request(&pid_file, &helper_binary, 120);
    let worker_connection = Arc::clone(&fixture.connection);
    let context = invocation_context(&fixture.workspace, "windows-cancel-tree");
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result =
            worker_connection.invoke_with_context(RUN_COMMAND_OPERATION, &request, &context);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(
        &pid_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    let command_pid = wait_for_pid(
        &fixture.workspace.join("command.pid"),
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(10),
    );
    assert!(process_exists(command_pid));
    assert!(process_exists(background_pid));
    fixture.connection.cancel_session("session-a").unwrap();
    let _ = finished_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("Windows command 取消后调用线程未及时结束");
    worker.join().unwrap();
    wait_for_process_exit(command_pid, Duration::from_secs(10));
    wait_for_process_exit(background_pid, Duration::from_secs(10));
}

#[cfg(windows)]
#[test]
fn real_launcher_timeout_kills_windows_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let helper_binary = copy_current_test_binary(&fixture.workspace, "timeout-tree-helper");
    std::fs::write(fixture.workspace.join(WINDOWS_PROCESS_TREE_MARKER), "ready").unwrap();
    let pid_file = fixture.workspace.join("background.pid");
    let request = windows_background_process_request(&pid_file, &helper_binary, 10);
    let worker_connection = Arc::clone(&fixture.connection);
    let context = invocation_context(&fixture.workspace, "windows-timeout-tree");
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result =
            worker_connection.invoke_with_context(RUN_COMMAND_OPERATION, &request, &context);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(
        &pid_file,
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(30),
    );
    let command_pid = wait_for_pid(
        &fixture.workspace.join("command.pid"),
        &finished_rx,
        &fixture.sidecar_log,
        Duration::from_secs(10),
    );
    let raw = finished_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("Windows command 超时后调用线程未及时结束")
        .expect("Windows command 超时应返回结构化响应");
    worker.join().unwrap();
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false, "超时命令不应成功: {raw}");
    assert!(
        response["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("超时"),
        "超时响应应明确说明原因: {raw}"
    );
    wait_for_process_exit(command_pid, Duration::from_secs(10));
    wait_for_process_exit(background_pid, Duration::from_secs(10));
}

#[cfg(target_os = "macos")]
#[test]
fn real_launcher_host_crash_kills_busy_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !sandbox_binaries_ready() {
        return;
    }
    let Some(host_binary) = debug_binary("test-stdio-host") else {
        eprintln!("跳过真实宿主崩溃测试：target/debug/test-stdio-host 尚未构建");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let host_log = root.path().join("host.log");
    let stderr = std::fs::File::create(&host_log).unwrap();
    let host = std::process::Command::new(host_binary)
        .arg("--command-sandbox")
        .arg(command_sidecar_binary().unwrap())
        .arg(root.path())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap();
    let mut guard = MacHostGuard {
        host,
        sidecar_pid: None,
    };
    let sidecar_pid = wait_for_host_pid_file(
        &root.path().join("workspace/sidecar.pid"),
        &mut guard.host,
        &host_log,
        Duration::from_secs(30),
    );
    let child_pid = wait_for_host_pid_file(
        &root.path().join("workspace/child.pid"),
        &mut guard.host,
        &host_log,
        Duration::from_secs(30),
    );
    guard.sidecar_pid = Some(sidecar_pid);
    assert!(process_exists(sidecar_pid));
    assert!(process_exists(child_pid));

    unsafe { libc::kill(guard.host.id() as libc::pid_t, libc::SIGKILL) };
    let _ = guard.host.wait();
    wait_for_process_exit(sidecar_pid, Duration::from_secs(5));
    wait_for_process_exit(child_pid, Duration::from_secs(5));
    guard.sidecar_pid = None;
    eprintln!("SAFE: macos_host_crash_process_tree");
}

#[cfg(target_os = "macos")]
struct MacHostGuard {
    host: std::process::Child,
    sidecar_pid: Option<i32>,
}

#[cfg(target_os = "macos")]
impl Drop for MacHostGuard {
    fn drop(&mut self) {
        unsafe { libc::kill(self.host.id() as libc::pid_t, libc::SIGKILL) };
        let _ = self.host.wait();
        if let Some(pid) = self.sidecar_pid {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_for_host_pid_file(
    path: &Path,
    host: &mut std::process::Child,
    log: &Path,
    timeout: Duration,
) -> i32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<i32>()
        {
            return pid;
        }
        if let Some(status) = host.try_wait().expect("读取测试宿主状态失败") {
            panic!(
                "PID 文件就绪前测试宿主已退出: {status}；日志:\n{}",
                read_log(log)
            );
        }
        assert!(
            Instant::now() < deadline,
            "等待测试宿主 PID 文件超时；日志:\n{}",
            read_log(log)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
#[test]
fn real_launcher_host_crash_kills_busy_process_tree() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !sandbox_binaries_ready() {
        return;
    }
    let Some(host_binary) = debug_binary("test-stdio-host") else {
        eprintln!("跳过 Windows 真实宿主崩溃测试：target/debug/test-stdio-host 尚未构建");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let host_log = root.path().join("host.log");
    let stderr = std::fs::File::create(&host_log).unwrap();
    let host = std::process::Command::new(host_binary)
        .arg("--command-sandbox")
        .arg(command_sidecar_binary().unwrap())
        .arg(root.path())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap();
    let mut guard = WindowsHostGuard {
        host,
        tracked_pids: Vec::new(),
    };
    let workspace = root.path().join("workspace");
    let command_pid = wait_for_windows_host_pid_file(
        &workspace.join("command.pid"),
        &mut guard.host,
        &host_log,
        Duration::from_secs(45),
    );
    let child_pid = wait_for_windows_host_pid_file(
        &workspace.join("child.pid"),
        &mut guard.host,
        &host_log,
        Duration::from_secs(10),
    );
    let sidecar_pid = windows_parent_pid(command_pid).expect("读取 command sidecar PID 失败");
    let launcher_pid = windows_parent_pid(sidecar_pid).expect("读取 Sandbox Launcher PID 失败");
    guard.tracked_pids = vec![launcher_pid, sidecar_pid, command_pid, child_pid];
    for pid in &guard.tracked_pids {
        assert!(process_exists(*pid), "宿主崩溃前进程不存在: {pid}");
    }

    guard.host.kill().expect("强制结束 Windows 测试宿主失败");
    let _ = guard.host.wait();
    for pid in &guard.tracked_pids {
        wait_for_process_exit(*pid, Duration::from_secs(10));
    }
    guard.tracked_pids.clear();
    eprintln!("SAFE: windows_host_crash_process_tree");
}

#[cfg(windows)]
struct WindowsHostGuard {
    host: std::process::Child,
    tracked_pids: Vec<u32>,
}

#[cfg(windows)]
impl Drop for WindowsHostGuard {
    fn drop(&mut self) {
        let _ = self.host.kill();
        let _ = self.host.wait();
        for pid in &self.tracked_pids {
            terminate_windows_process(*pid);
        }
    }
}

#[cfg(windows)]
fn wait_for_windows_host_pid_file(
    path: &Path,
    host: &mut std::process::Child,
    log: &Path,
    timeout: Duration,
) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        if let Some(status) = host.try_wait().expect("读取 Windows 测试宿主状态失败") {
            panic!(
                "PID 文件就绪前 Windows 测试宿主已退出: {status}；日志:\n{}",
                read_log(log)
            );
        }
        assert!(
            Instant::now() < deadline,
            "等待 Windows 测试宿主 PID 文件超时；日志:\n{}",
            read_log(log)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn terminate_windows_process(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return;
    }
    unsafe {
        TerminateProcess(handle, 1);
        CloseHandle(handle);
    }
}

#[cfg(windows)]
fn windows_parent_pid(pid: u32) -> std::io::Result<u32> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    loop {
        if entry.th32ProcessID == pid {
            return Ok(entry.th32ParentProcessID);
        }
        if unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } == 0 {
            break;
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("Windows 进程快照中未找到 PID {pid}"),
    ))
}

#[cfg(target_os = "linux")]
#[test]
fn real_launcher_applies_resource_limits() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let response = invoke_shell(
        &fixture,
        "resource-limits",
        "/bin/bash -c 'ulimit -t; ulimit -v; ulimit -u'".to_string(),
    );
    assert_eq!(response["ok"], true, "资源上限探测失败: {response}");
    let values = response["stdout"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        ["300", "2097152", "64"],
        "资源上限未生效: {response}"
    );
}

#[cfg(any(unix, windows))]
#[test]
#[ignore = "由 Sandbox CI 在独立进程中运行，避免与串行攻击矩阵共享平台资源"]
fn ten_concurrent_commands_can_be_cancelled_and_stopped() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let mut workers = Vec::new();
    let mut markers = Vec::new();
    #[cfg(windows)]
    std::fs::write(fixture.workspace.join(WINDOWS_CONCURRENT_MARKER), "ready").unwrap();
    for index in 0..10 {
        let marker = fixture.workspace.join(format!("concurrent-{index}.ready"));
        markers.push(marker.clone());
        #[cfg(unix)]
        let request = shell_request_with_timeout(
            format!(": > {}; kill -STOP $$", shell_quote(&marker)),
            &fixture.workspace,
            120,
        );
        #[cfg(windows)]
        let request = {
            let helper =
                copy_current_test_binary(&fixture.workspace, &format!("concurrent-{index}"));
            windows_command_request(
                &helper,
                "windows_concurrent_wait_helper",
                &fixture.workspace,
                120,
            )
        };
        #[cfg(unix)]
        let operation = RUN_SHELL_OPERATION;
        #[cfg(windows)]
        let operation = RUN_COMMAND_OPERATION;
        let connection = Arc::clone(&fixture.connection);
        let context = invocation_context(&fixture.workspace, &format!("concurrent-{index}"));
        let finished_tx = finished_tx.clone();
        workers.push(std::thread::spawn(move || {
            let result = connection.invoke_with_context(operation, &request, &context);
            let _ = finished_tx.send((index, result));
        }));
    }
    drop(finished_tx);

    let deadline = Instant::now() + Duration::from_secs(90);
    while !markers.iter().all(|marker| marker.is_file()) {
        if let Ok((index, result)) = finished_rx.try_recv() {
            panic!("并发 command {index} 在取消前提前结束: {result:?}");
        }
        if Instant::now() >= deadline {
            let started = markers.iter().filter(|marker| marker.is_file()).count();
            #[cfg(target_os = "linux")]
            let pre_stop_diagnostics = format!(
                "停止前进程:\n{}\n\n停止前 sidecar 日志:\n{}",
                concurrent_process_snapshot(),
                concurrent_sidecar_logs()
            );
            #[cfg(not(target_os = "linux"))]
            let pre_stop_diagnostics = "当前平台未收集 Linux 进程诊断".to_string();
            let stop_result = fixture.connection.stop();
            let mut diagnostics = Vec::new();
            for _ in 0..workers.len() {
                match finished_rx.recv_timeout(Duration::from_secs(10)) {
                    Ok((index, Ok(response))) => {
                        diagnostics.push(format!("command {index}: {response}"));
                    }
                    Ok((index, Err(error))) => {
                        diagnostics.push(format!("command {index}: {error:#}"));
                    }
                    Err(error) => {
                        diagnostics.push(format!("等待 command 结束失败: {error}"));
                        break;
                    }
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
            panic!(
                "并发 command 仅启动 {started}/10；{pre_stop_diagnostics}\n\n停止结果: {stop_result:?}；调用诊断:\n{}",
                diagnostics.join("\n\n")
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let cancel_connection = Arc::clone(&fixture.connection);
    let stop_connection = Arc::clone(&fixture.connection);
    let cancel = std::thread::spawn(move || cancel_connection.cancel_session("session-a"));
    let stop = std::thread::spawn(move || stop_connection.stop());
    cancel.join().unwrap().unwrap();
    stop.join().unwrap().unwrap();

    for _ in 0..10 {
        let (_, result) = finished_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("并发 command 在取消后未及时结束");
        result.ok();
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn execution_without_invocation_context_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let config = SidecarConfig::new(
        "command",
        "0.0.0",
        root.path().join("missing-sidecar"),
        root.path().join("endpoint.json"),
        root.path().join("sidecar.log"),
        root.path().join("data"),
        root.path(),
    );
    let connection = EphemeralCommandConnection::new(config);
    let error = connection
        .invoke(RUN_SHELL_OPERATION, r#"{"script":"echo x"}"#)
        .unwrap_err();
    assert!(error.to_string().contains("缺少宿主工具调用上下文"));
}

#[cfg(any(unix, windows))]
#[test]
fn stdio_stop_kills_background_process_tree() {
    let Some(binary) = command_sidecar_binary() else {
        eprintln!("跳过进程组测试：target/debug/tiangong-command-sidecar 尚未构建");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let sidecar_log = root.path().join("sidecar.log");
    std::fs::create_dir_all(&workspace).unwrap();
    let connection = Arc::new(StdioSidecarConnection::new(
        SidecarConfig::new(
            "command",
            PLUGIN_VERSION,
            binary,
            root.path().join("endpoint.json"),
            sidecar_log.clone(),
            root.path().join("data"),
            root.path().join("storage"),
        )
        .with_protocols(PROTOCOL_VERSION, COMMAND_PROTOCOL_VERSION)
        .with_timeouts(Duration::from_secs(15), Duration::from_secs(30))
        .with_lifecycle(SidecarLifecycle::Resident),
    ));
    connection
        .invoke(
            SET_WORKSPACE_OPERATION,
            &serde_json::json!({
                "workspace": workspace.display().to_string(),
                "full_trust": true,
                "allowed_commands": [],
            })
            .to_string(),
        )
        .unwrap();

    let pid_file = workspace.join("background.pid");
    #[cfg(unix)]
    let heartbeat_file = workspace.join("background.heartbeat");
    #[cfg(unix)]
    let request = shell_request(
        unix_background_process_script(&pid_file, &heartbeat_file),
        &workspace,
    );
    #[cfg(windows)]
    std::fs::write(workspace.join(WINDOWS_PROCESS_TREE_MARKER), b"ready").unwrap();
    #[cfg(windows)]
    let helper_binary = std::env::current_exe().expect("读取 Windows 测试程序路径失败");
    #[cfg(windows)]
    let request = windows_background_process_request(&pid_file, &helper_binary, 120);
    #[cfg(unix)]
    let operation = RUN_SHELL_OPERATION;
    #[cfg(windows)]
    let operation = RUN_COMMAND_OPERATION;
    let worker_connection = Arc::clone(&connection);
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = worker_connection.invoke(operation, &request);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(
        &pid_file,
        &finished_rx,
        &sidecar_log,
        Duration::from_secs(35),
    );
    #[cfg(unix)]
    wait_for_heartbeat_growth(
        &heartbeat_file,
        &finished_rx,
        &sidecar_log,
        Duration::from_secs(30),
    );
    assert!(process_exists(background_pid));
    connection.stop().unwrap();
    let _ = finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停止 sidecar 后执行线程未及时结束");
    worker.join().unwrap();
    wait_for_process_exit(background_pid, Duration::from_secs(5));
    #[cfg(unix)]
    wait_for_heartbeat_stop(&heartbeat_file, Duration::from_secs(5));
}

#[cfg(any(unix, windows))]
#[test]
fn repeated_stdio_start_stop_does_not_leak_host_resources() {
    let Some(binary) = command_sidecar_binary() else {
        eprintln!("跳过资源泄漏测试：target/debug/tiangong-command-sidecar 尚未构建");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let start_and_stop = |index: &str| {
        let instance = root.path().join(format!("instance-{index}"));
        let connection = StdioSidecarConnection::new(
            SidecarConfig::new(
                "command",
                PLUGIN_VERSION,
                &binary,
                instance.join("endpoint.json"),
                instance.join("sidecar.log"),
                instance.join("data"),
                instance.join("storage"),
            )
            .with_protocols(PROTOCOL_VERSION, COMMAND_PROTOCOL_VERSION)
            .with_timeouts(Duration::from_secs(15), Duration::from_secs(15))
            .with_lifecycle(SidecarLifecycle::Resident),
        );
        connection
            .invoke(
                SET_WORKSPACE_OPERATION,
                &serde_json::json!({
                    "workspace": workspace.display().to_string(),
                    "full_trust": true,
                    "allowed_commands": [],
                })
                .to_string(),
            )
            .unwrap();
        connection.stop().unwrap();
    };

    // 首次 stdio 调用会初始化进程级全局设施；预热后再取基线，避免把
    // 一次性常驻句柄误判为逐次泄漏。
    start_and_stop("warmup");
    std::thread::sleep(Duration::from_millis(500));
    let before = host_resource_count();

    for index in 0..20 {
        start_and_stop(&index.to_string());
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let after = loop {
        let current = host_resource_count();
        if current <= before + 10 || Instant::now() >= deadline {
            break current;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        after <= before + 10,
        "重复启动停止后宿主资源持续增长: before={before}, after={after}"
    );
}

#[cfg(unix)]
fn unix_background_process_script(pid_file: &Path, heartbeat_file: &Path) -> String {
    format!(
        "(while :; do /usr/bin/printf x >> {}; /bin/sleep 0.1; done) & child=$!; /usr/bin/printf '%s' \"$child\" > {}; wait \"$child\"",
        shell_quote(heartbeat_file),
        shell_quote(pid_file),
    )
}

#[cfg(windows)]
fn windows_background_process_request(
    pid_file: &Path,
    executable: &Path,
    timeout_secs: u64,
) -> String {
    windows_command_request(
        executable,
        "windows_process_tree_helper",
        pid_file.parent().expect("PID 文件必须位于工作区内"),
        timeout_secs,
    )
}

#[cfg(windows)]
fn collect_windows_sandbox_probe(
    request: &WindowsSandboxProbeRequest,
    suffix: &str,
) -> WindowsSandboxProbeReport {
    let temp_dirs = ["TMPDIR", "TMP", "TEMP"]
        .map(std::env::var_os)
        .map(|value| value.map(PathBuf::from));
    let same_temp = temp_dirs
        .iter()
        .all(|path| path.as_ref() == temp_dirs[0].as_ref());
    let dedicated_temp_write = temp_dirs[0].as_ref().is_some_and(|temp| {
        same_temp
            && temp.is_absolute()
            && temp.ancestors().any(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("tiangong-command-"))
            })
            && std::fs::write(temp.join(format!("probe-{suffix}.tmp")), "ok").is_ok()
    });
    WindowsSandboxProbeReport {
        workspace_write: std::fs::write(
            request.workspace.join(format!("inside-{suffix}.txt")),
            "ok",
        )
        .is_ok(),
        existing_workspace_read_write: std::fs::read_to_string(&request.existing_workspace_file)
            .is_ok_and(|value| matches!(value.as_str(), "workspace-before" | "workspace-after"))
            && std::fs::write(&request.existing_workspace_file, "workspace-after").is_ok(),
        dedicated_temp_write,
        outside_write_blocked: std::fs::write(&request.outside_write, "PWNED").is_err(),
        outside_read_blocked: std::fs::read(&request.outside_read).is_err(),
        outside_delete_blocked: std::fs::remove_dir_all(&request.outside_delete).is_err(),
        outside_config_write_blocked: std::fs::write(&request.outside_config, "PWNED").is_err(),
        git_metadata_write_blocked: std::fs::write(&request.git_config, "PWNED").is_err()
            && request
                .git_config
                .parent()
                .is_some_and(|git| std::fs::remove_dir_all(git).is_err()),
        git_metadata_readable: std::fs::read_to_string(&request.git_config)
            .is_ok_and(|value| value == "safe\n"),
        network_blocked: request.network_address.parse().ok().is_some_and(|address| {
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err()
        }),
        sensitive_read_blocked: request
            .sensitive_paths
            .iter()
            .all(|path| std::fs::read(path).is_err()),
        path_traversal_blocked: std::fs::write(&request.traversal_target, "PWNED").is_err(),
        policy_envelope_hidden: [
            tiangong_sandbox::POLICY_ENV,
            tiangong_sandbox::WINDOWS_STOP_EVENT_ENV,
            tiangong_sandbox::HOST_PID_ENV,
        ]
        .iter()
        .all(|key| std::env::var_os(key).is_none()),
        resource_limits_applied: tiangong_sandbox::sandbox::windows::current_process_limits_match(
            request.resource_limits,
        ),
        child_restrictions_inherited: false,
    }
}

#[cfg(windows)]
fn read_windows_sandbox_probe_request() -> Option<WindowsSandboxProbeRequest> {
    let path = std::env::current_dir()
        .ok()?
        .join(WINDOWS_SANDBOX_PROBE_REQUEST);
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(windows)]
#[test]
fn windows_command_sandbox_probe_helper() {
    let Some(request) = read_windows_sandbox_probe_request() else {
        return;
    };
    let mut report = collect_windows_sandbox_probe(&request, "parent");
    let child_ok = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "windows_command_sandbox_child_probe_helper",
            "--exact",
            "--nocapture",
        ])
        .current_dir(&request.workspace)
        .status()
        .is_ok_and(|status| status.success());
    report.child_restrictions_inherited = child_ok
        && std::fs::read(&request.child_report_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<WindowsSandboxProbeReport>(&raw).ok())
            .is_some_and(|child| {
                child.workspace_write
                    && child.existing_workspace_read_write
                    && child.dedicated_temp_write
                    && child.outside_write_blocked
                    && child.outside_read_blocked
                    && child.outside_delete_blocked
                    && child.outside_config_write_blocked
                    && child.git_metadata_write_blocked
                    && child.git_metadata_readable
                    && child.network_blocked
                    && child.sensitive_read_blocked
                    && child.path_traversal_blocked
                    && child.policy_envelope_hidden
                    && child.resource_limits_applied
            });
    std::fs::write(&request.report_path, serde_json::to_vec(&report).unwrap())
        .expect("写入 Windows command Sandbox 探针报告失败");
}

#[cfg(windows)]
#[test]
fn windows_command_sandbox_child_probe_helper() {
    let Some(request) = read_windows_sandbox_probe_request() else {
        return;
    };
    let report = collect_windows_sandbox_probe(&request, "child");
    std::fs::write(
        &request.child_report_path,
        serde_json::to_vec(&report).unwrap(),
    )
    .expect("写入 Windows command 子进程探针报告失败");
}

#[cfg(windows)]
#[test]
fn windows_concurrent_wait_helper() {
    let workspace = std::env::current_dir().expect("读取 Windows 并发 helper 工作目录失败");
    if !workspace.join(WINDOWS_CONCURRENT_MARKER).is_file() {
        return;
    }
    let executable = std::env::current_exe().expect("读取 Windows 并发 helper 路径失败");
    let name = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .expect("Windows 并发 helper 文件名无效");
    std::fs::write(workspace.join(format!("{name}.ready")), "ready")
        .expect("写入 Windows 并发 helper 就绪标记失败");
    std::thread::sleep(Duration::from_secs(300));
}

#[cfg(windows)]
#[test]
fn windows_process_tree_helper() {
    let workspace = std::env::current_dir().expect("读取 Windows helper 工作目录失败");
    if !workspace.join(WINDOWS_PROCESS_TREE_MARKER).is_file() {
        return;
    }

    std::fs::write(
        workspace.join("command.pid"),
        std::process::id().to_string(),
    )
    .expect("写入 Windows command 进程 PID 失败");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["windows_wait_forever_helper", "--exact", "--nocapture"])
        .current_dir(&workspace)
        .spawn()
        .expect("启动 Windows 后台进程失败");
    std::fs::write(workspace.join("background.pid"), child.id().to_string())
        .expect("写入 Windows 后台进程 PID 失败");
    child.wait().expect("等待 Windows 后台进程失败");
}

#[cfg(windows)]
#[test]
fn windows_wait_forever_helper() {
    let workspace = std::env::current_dir().expect("读取 Windows helper 工作目录失败");
    if !workspace.join(WINDOWS_PROCESS_TREE_MARKER).is_file() {
        return;
    }
    std::thread::sleep(Duration::from_secs(300));
}

#[cfg(unix)]
fn wait_for_pid(
    path: &Path,
    finished: &Receiver<anyhow::Result<String>>,
    log: &Path,
    timeout: Duration,
) -> i32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<i32>()
        {
            return pid;
        }
        ensure_invocation_running(finished, log);
        if Instant::now() >= deadline {
            panic!(
                "等待后台进程 PID 超时；调用诊断:\n{}",
                invocation_diagnostics(log)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn wait_for_pid(
    path: &Path,
    finished: &Receiver<anyhow::Result<String>>,
    log: &Path,
    timeout: Duration,
) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        ensure_invocation_running(finished, log);
        if Instant::now() >= deadline {
            panic!("等待后台进程 PID 超时；sidecar 日志:\n{}", read_log(log));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ensure_invocation_running(finished: &Receiver<anyhow::Result<String>>, log: &Path) {
    match finished.try_recv() {
        Ok(Ok(response)) => panic!(
            "后台进程就绪前 command 已返回: {response}；调用诊断:\n{}",
            invocation_diagnostics(log)
        ),
        Ok(Err(error)) => panic!(
            "后台进程就绪前 command 调用失败: {error:#}；调用诊断:\n{}",
            invocation_diagnostics(log)
        ),
        Err(TryRecvError::Disconnected) => panic!(
            "后台进程就绪前 command 执行线程已断开；调用诊断:\n{}",
            invocation_diagnostics(log)
        ),
        Err(TryRecvError::Empty) => {}
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("<读取失败: {error}>"))
}

#[cfg(unix)]
fn host_resource_count() -> u32 {
    let path = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(path)
        .map(|entries| entries.filter_map(Result::ok).count() as u32)
        .expect("读取宿主文件描述符失败")
}

#[cfg(windows)]
fn host_resource_count() -> u32 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    let mut count = 0;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    assert_ne!(ok, 0, "读取宿主句柄数量失败");
    count
}

#[cfg(unix)]
fn wait_for_heartbeat_growth(
    path: &Path,
    finished: &Receiver<anyhow::Result<String>>,
    log: &Path,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut first_len = None;
    loop {
        if let Ok(metadata) = std::fs::metadata(path) {
            let len = metadata.len();
            if first_len.is_some_and(|first| len > first) {
                return;
            }
            if len > 0 {
                first_len.get_or_insert(len);
            }
        }
        ensure_invocation_running(finished, log);
        if Instant::now() >= deadline {
            panic!(
                "后台进程心跳未增长；调用诊断:\n{}",
                invocation_diagnostics(log)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn wait_for_heartbeat_stop(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_len = std::fs::metadata(path)
        .map(|value| value.len())
        .unwrap_or(0);
    let mut stable_since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let len = std::fs::metadata(path)
            .map(|value| value.len())
            .unwrap_or(0);
        if len == last_len {
            if stable_since.elapsed() >= Duration::from_millis(500) {
                return;
            }
        } else {
            last_len = len;
            stable_since = Instant::now();
        }
        assert!(Instant::now() < deadline, "后台进程在清理后仍持续运行");
    }
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    queried && exit_code == STILL_ACTIVE as u32
}

#[cfg(unix)]
fn wait_for_process_exit(pid: i32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while process_exists(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!process_exists(pid), "后台进程未随 sidecar 一起结束: {pid}");
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while process_exists(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!process_exists(pid), "后台进程未随 sidecar 一起结束: {pid}");
}
