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
    git_metadata_write: bool,
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
    sandbox_fixture_with_limits(None)
}

/// Launcher 验签测试密钥（静态，CI 测试密钥同源语义）：只写入测试 fixture
/// 的临时存储根，无法冒充编译进宿主的官方信任根。Ed25519 签名确定性使
/// nextest 并发下重复写同一签名文件内容一致，无竞态。
#[cfg(any(unix, windows))]
const TEST_SIGNING_SECRET_KEY: &str = "untrusted comment: rsign encrypted secret key\nRWQAAEIyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAls4pXFjc5mdI4GLK03g7o1W1/i77lDVt6N0UEBq1kiaRr4Tu8k4qp6+g7ZVZTZVY46bgfgMqxzLtK9cQwyK7Tjh7/3zxhx5Q4jk/sWXImXvRS/pwCH3EFfNivwZFLOJkLCbVcQ2/qz4=";
#[cfg(any(unix, windows))]
const TEST_SIGNING_PUBLIC_KEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDY3RTZEQzU4NUMyOUNFOTYKUldTV3ppbGNXTnptWjYrZzdaVlpUWlZZNDZiZ2ZnTXF4ekx0SzljUXd5SzdUamg3LzN6eGh4NVEK";

/// 把 debug Launcher 复制到测试进程目录（launcher_manager 的优先解析位置）
/// 并用测试密钥签名，同时把测试公钥写入 fixture 存储根作为本机用户密钥
/// 信任根——真实链路因此完整经过"启动前验签"。
#[cfg(any(unix, windows))]
fn ensure_test_launcher_signed(storage_root: &Path) {
    let launcher_source = debug_binary("tiangong-sandbox").expect("sandbox_binaries_ready 已确认");
    let test_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let launcher = test_dir.join(format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX));
    if std::fs::read(&launcher).ok() != std::fs::read(&launcher_source).ok() {
        std::fs::copy(&launcher_source, &launcher).unwrap();
    }
    let secret_box =
        minisign::SecretKeyBox::from_string(TEST_SIGNING_SECRET_KEY).expect("测试私钥格式无效");
    let secret = secret_box
        .into_unencrypted_secret_key()
        .expect("测试私钥加载失败");
    let public = minisign::PublicKey::from_secret_key(&secret).unwrap();
    let data = std::fs::read(&launcher).unwrap();
    let signature = minisign::sign(Some(&public), &secret, data.as_slice(), None, None).unwrap();
    let sig_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(signature.into_string())
    };
    std::fs::write(
        launcher.with_file_name(format!(
            "tiangong-sandbox{}.sig",
            std::env::consts::EXE_SUFFIX
        )),
        sig_b64,
    )
    .unwrap();

    let keys = storage_root.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    std::fs::write(
        keys.join("user-signing.key.pub"),
        format!("{TEST_SIGNING_PUBLIC_KEY_B64}\n"),
    )
    .unwrap();
}

#[cfg(any(unix, windows))]
fn sandbox_fixture_with_limits(
    limits: Option<tiangong_sandbox::SandboxResourceLimits>,
) -> Option<SandboxFixture> {
    sandbox_fixture_configured(true, limits)
}

/// `sandbox=false` 模拟用户在设置中关闭命令沙箱：不经 Launcher 直跑，
/// 不依赖平台沙箱可用性（嵌套受限环境也可真实执行）。
#[cfg(any(unix, windows))]
fn sandbox_fixture_configured(
    sandbox: bool,
    limits: Option<tiangong_sandbox::SandboxResourceLimits>,
) -> Option<SandboxFixture> {
    if sandbox {
        if !sandbox_binaries_ready() {
            return None;
        }
    } else if command_sidecar_binary().is_none() {
        eprintln!("跳过无沙箱测试：target/debug/tiangong-command-sidecar 尚未构建");
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let storage_root = root.path().join("storage");
    if sandbox {
        ensure_test_launcher_signed(&storage_root);
    } else {
        // 沙箱关闭路径：初始化全局配置写入 sandbox_disabled=true（宿主
        // 执行器按全局开关分流）；测试退出前由恢复钩子还原为默认配置，
        // 避免污染后续沙箱用例。
        let config_dir = root.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("app.json"), r#"{"sandbox_disabled":true}"#).unwrap();
        tiangong_config::registry::init_from_dir(&config_dir);
    }
    let plugin_root = storage_root.join("plugins/command");
    let workspace = storage_root.join("workspaces/session-a");
    // 工作区外标记与伪造 HOME 默认落真实 HOME 下；外层受限沙箱
    //（如天工终端）可能拒绝写真实 HOME，预检失败时回退测试临时目录
    //（仍在 workspace 之外，断言语义不变；sidecar 的 HOME 经环境变量
    // 注入 fake_home，与真实 HOME 可写性无关）。
    let writable_home_subdir = |sub: &str| -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let dir = PathBuf::from(home).join(sub);
        std::fs::create_dir_all(&dir).ok()?;
        // 目录可能是历史运行遗留（create_dir_all 对已存在目录不发生写入），
        // 必须真实写文件探测：外层受限沙箱拒绝的是 file-write。
        let probe = dir.join(".write-probe");
        std::fs::write(&probe, b"").ok()?;
        let _ = std::fs::remove_file(&probe);
        Some(dir)
    };
    let outside = writable_home_subdir(".tiangong-test-outside")
        .map(|dir| dir.join("blocked.txt"))
        .unwrap_or_else(|| root.path().join("outside/blocked.txt"));
    let fake_home =
        writable_home_subdir(".tiangong-test-fakehome").unwrap_or_else(|| root.path().join("home"));
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
    .with_sandbox(sandbox)
    .with_sandbox_program_root(Some(plugin_root))
    .with_sandbox_denied_read_paths(vec![ssh_dir, aws_dir, trust_db.clone()])
    .with_sandbox_resource_limits(limits);
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

#[cfg(unix)]
#[test]
fn sandbox_disabled_runs_command_without_launcher() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 关闭命令沙箱：仍由独立 Command Sidecar 执行业务，只是不经 Launcher；
    // 因而具有完整文件权限，同时保留插件自身的环境清理、超时与进程树清理。
    let Some(fixture) = sandbox_fixture_configured(false, None) else {
        restore_default_config();
        return;
    };
    let outside_marker = fixture
        .outside
        .parent()
        .unwrap()
        .join("command-sidecar-marker.txt");

    let probe = format!(
        "printf direct > {ws}/ran.txt && printf full > {outside} && printf '%s' \"${{NODE_OPTIONS:-cleared}}\"",
        ws = fixture.workspace.display(),
        outside = outside_marker.display(),
    );
    // 测试进程单线程（串行锁保护）下设置环境变量安全。
    unsafe { std::env::set_var("NODE_OPTIONS", "should-be-cleared") };
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &shell_request(probe, &fixture.workspace),
            &invocation_context(&fixture.workspace, "sandbox-disabled"),
        )
        .expect("Command Sidecar 命令失败");
    unsafe { std::env::remove_var("NODE_OPTIONS") };
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], true, "关闭沙箱后命令应真实执行: {raw}");
    assert_eq!(
        response["stdout"].as_str().unwrap_or_default(),
        "cleared",
        "Command Sidecar 仍应清理解释器注入类环境变量: {raw}"
    );
    assert!(fixture.workspace.join("ran.txt").is_file());
    assert!(
        outside_marker.is_file(),
        "完整用户权限：工作区外可写（Command Sidecar）"
    );

    // 进程树清理：短超时 + 后台 sleep，超时后整组终止、无遗留进程。
    //（清理依赖向子进程发送信号：不能终止子进程的环境该段不可验证，
    // 前半段的命令执行与环境变量清理断言不受影响。）
    if !tiangong_plugin_runtime::test_support::can_terminate_child_processes() {
        restore_default_config();
        return;
    }
    let sleeper = format!(
        "/bin/sleep 30 & echo $! > {ws}/sleep.pid; wait",
        ws = fixture.workspace.display(),
    );
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &shell_request_with_timeout(sleeper, &fixture.workspace, 1),
            &invocation_context(&fixture.workspace, "sandbox-disabled-timeout"),
        )
        .expect("超时路径应返回响应而非错误");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false, "超时应失败: {raw}");
    let sleep_pid: i32 = std::fs::read_to_string(fixture.workspace.join("sleep.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut gone = false;
    for _ in 0..20 {
        if !std::path::Path::new(&format!("/proc/{sleep_pid}")).exists()
            && unsafe { libc::kill(sleep_pid, 0) } != 0
        {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(gone, "超时后后台 sleep 应被进程组清理（pid={sleep_pid}）");
    restore_default_config();
}

/// 还原全局配置为默认（sandbox_disabled=false），避免关闭态泄漏到后续用例。
fn restore_default_config() {
    let dir = tempfile::tempdir().unwrap();
    tiangong_config::registry::init_from_dir(dir.path());
}

#[cfg(unix)]
#[test]
fn real_launcher_enforces_resource_limits() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) =
        sandbox_fixture_with_limits(Some(tiangong_sandbox::SandboxResourceLimits {
            max_cpu_time_seconds: 1,
            max_memory_bytes: 512 * 1024 * 1024,
            max_processes: 64,
        }))
    else {
        return;
    };

    // CPU 时间上限 1 秒：死循环在命令级超时（15 秒）之前被 SIGXCPU 强制
    // 终止，证明上限在真实链路上强制施加，不依赖目标进程自觉配合。
    let started = Instant::now();
    let raw = fixture
        .connection
        .invoke_with_context(
            RUN_SHELL_OPERATION,
            &shell_request_with_timeout(
                "i=0; while :; do i=$((i+1)); done".to_string(),
                &fixture.workspace,
                15,
            ),
            &invocation_context(&fixture.workspace, "cpu-limit"),
        )
        .expect("真实 Launcher 应返回 command 失败响应");
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["ok"], false, "CPU 上限应强制终止死循环: {raw}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "死循环应在命令级超时前被 CPU 上限终止，实际耗时 {}",
        started.elapsed().as_secs()
    );
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
        report.git_metadata_write,
        "Git 元数据写入未放行: {report:?}"
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

    // .git 已开放（用户裁定 2026-08-30）：agent 需要完整 git 工作流。
    // 工作区内的 .git 与源码同等对待——写入必须放行且真实落盘。
    let git_dir = fixture.workspace.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let git_config = git_dir.join("config");
    std::fs::write(&git_config, "safe\n").unwrap();
    let response = invoke_shell(
        &fixture,
        "write_git_config",
        format!("printf GITWRITE > {}", shell_quote(&git_config)),
    );
    assert_eq!(response["ok"], true, "Git 元数据写入应放行: {response}");
    assert_eq!(
        std::fs::read_to_string(&git_config).unwrap(),
        "GITWRITE",
        "Git 元数据写入应真实落盘"
    );
    eprintln!("OK: write_git_config");

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

#[cfg(any(unix, windows))]
#[test]
fn real_launcher_sensitive_config_read_authorization() {
    let _serial = REAL_SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fixture) = sandbox_fixture_with_limits(None) else {
        return;
    };
    // storage_root = workspace(…/storage/workspaces/x) 上溯两级。
    let storage = fixture.workspace.parent().unwrap().parent().unwrap();
    let models = storage.join("models.json");
    let mcp = storage.join("mcp.json");
    std::fs::write(&models, r#"{"models":[]}"#).unwrap();
    std::fs::write(&mcp, r#"{"mcpServers":{}}"#).unwrap();
    // command 通道未获授权：模型/MCP 配置与凭据同列禁读（真实 Launcher 链）。
    for (name, path) in [("models", &models), ("mcp", &mcp)] {
        let raw = fixture
            .connection
            .invoke_with_context(
                RUN_SHELL_OPERATION,
                &shell_request(
                    format!("cat {} >/dev/null", shell_quote(path)),
                    &fixture.workspace,
                ),
                &invocation_context(&fixture.workspace, &format!("config-denied-{name}")),
            )
            .expect("配置读取调用失败");
        let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(response["ok"], false, "未授权读取 {name} 配置应失败: {raw}");
    }
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
        ("NODE_OPTIONS".into(), "PWNED_RUNTIME_NODE".into()),
        ("PYTHONPATH".into(), "PWNED_RUNTIME_PYTHON".into()),
        ("JAVA_TOOL_OPTIONS".into(), "PWNED_RUNTIME_JAVA".into()),
        ("ZDOTDIR".into(), "PWNED_RUNTIME_ZDOTDIR".into()),
    ]));
    std::fs::write(
        fixture.workspace.join(".env"),
        "LD_PRELOAD=PWNED_FILE_LD\nDYLD_INSERT_LIBRARIES=PWNED_FILE_DYLD\nBASH_ENV=PWNED_FILE_BASH\nPATH=PWNED_FILE_PATH\nTMPDIR=PWNED_FILE_TMP\nTMP=PWNED_FILE_TMP\nTEMP=PWNED_FILE_TMP\nNODE_OPTIONS=PWNED_FILE_NODE\nPYTHONPATH=PWNED_FILE_PYTHON\nPERL5OPT=PWNED_FILE_PERL\nRUBYOPT=PWNED_FILE_RUBY\n",
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
    // 进程数软上限无法降到低于当前会话已用进程数：共享 CI runner 上
    // setrlimit(RLIMIT_NPROC, 64) 会失败并保留硬上限（如 63796）。
    // 该环境下进程数限额属平台不可施加，只断言 CPU 与内存两项。
    #[cfg(unix)]
    let nproc_enforced = {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let probed =
            unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) } == 0 && limit.rlim_cur <= 64;
        if !probed {
            eprintln!(
                "跳过进程数上限断言：本环境 RLIMIT_NPROC 无法降到 64（硬上限 {}）",
                limit.rlim_max
            );
        }
        probed
    };
    #[cfg(not(unix))]
    let nproc_enforced = true;
    let expected_nproc = if nproc_enforced { "64" } else { values[2] };
    assert_eq!(
        values,
        ["300", "2097152", expected_nproc],
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
    // 断言依赖真实终止进程树：不能终止子进程的环境（外层受限沙箱拒
    // 信号）无法验证清理语义，整体跳过。
    if !tiangong_plugin_runtime::test_support::can_terminate_child_processes() {
        return;
    }
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
    // 每轮 stop 都必须真实终止常驻进程（stop 失败即测试失败）：
    // 不能终止子进程的环境整体跳过。
    if !tiangong_plugin_runtime::test_support::can_terminate_child_processes() {
        return;
    }
    let Some(binary) = command_sidecar_binary() else {
        eprintln!("跳过资源泄漏测试：target/debug/tiangong-command-sidecar 尚未构建");
        return;
    };
    // 每轮 stop 都必须真实终止常驻进程（stop 失败即测试失败）：
    // 受限环境拒信号时跳过。
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
        // .git 可写是功能要求（agent 需要完整 git 工作流）：写入探测标记
        // 并回读验证；父/子探针共用标记，重复探测幂等（初始 "safe\n"）。
        git_metadata_write: std::fs::write(&request.git_config, "GITWRITE").is_ok()
            && std::fs::read_to_string(&request.git_config).as_deref() == Ok("GITWRITE"),
        git_metadata_readable: std::fs::read_to_string(&request.git_config)
            .is_ok_and(|value| value == "safe\n" || value == "GITWRITE"),
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
                    && child.git_metadata_write
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
