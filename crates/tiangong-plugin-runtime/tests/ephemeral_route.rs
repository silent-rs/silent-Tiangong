//! command 一次性沙箱路由的真实执行验证。
//!
//! 真实 Launcher 用例依赖已构建的 `tiangong-sandbox` 与 command sidecar；
//! 平台不支持嵌套沙箱时明确跳过。进程组清理另用无沙箱 stdio 链路验证，
//! 避免受测试宿主自身沙箱状态影响。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use tiangong_plugin_runtime::sidecar::{
    EphemeralCommandConnection, SidecarConfig, SidecarConnection, SidecarInvocationContext,
    StdioSidecarConnection,
};

const RUN_SHELL_OPERATION: &str = "command.run_shell";
const SET_WORKSPACE_OPERATION: &str = "command.set_workspace";

fn debug_binary(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let debug_dir = executable.parent()?.parent()?;
    let candidate = debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn command_sidecar_binary() -> Option<PathBuf> {
    debug_binary("tiangong-command-sidecar")
}

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

struct SandboxFixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    connection: Arc<EphemeralCommandConnection>,
}

fn sandbox_fixture() -> Option<SandboxFixture> {
    if !sandbox_binaries_ready() {
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let storage_root = root.path().join("storage");
    let plugin_root = storage_root.join("plugins/command");
    let workspace = storage_root.join("workspaces/session-a");
    let outside = root.path().join("outside/blocked.txt");
    std::fs::create_dir_all(&plugin_root).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();

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

    let config = SidecarConfig::new(
        "command",
        "0.0.0",
        binary,
        plugin_root.join("unused-endpoint.json"),
        plugin_root.join("unused-sidecar.log"),
        plugin_root.join("unused-data"),
        storage_root,
    )
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(20))
    .with_sandbox_program_root(Some(plugin_root));

    Some(SandboxFixture {
        _root: root,
        workspace,
        outside,
        connection: Arc::new(EphemeralCommandConnection::new(config)),
    })
}

fn invocation_context(workspace: &Path, invocation_id: &str) -> SidecarInvocationContext {
    SidecarInvocationContext {
        session_id: "session-a".to_string(),
        invocation_id: invocation_id.to_string(),
        authoritative_workspace: workspace.to_path_buf(),
    }
}

fn shell_request(script: String, claimed_workspace: &Path) -> String {
    serde_json::json!({
        "script": script,
        "shell": "sh",
        "timeout_secs": 15,
        "workspace": claimed_workspace.display().to_string(),
        "full_trust": true,
        "allowed_commands": [],
    })
    .to_string()
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(unix)]
#[test]
fn real_launcher_enforces_workspace_and_dedicated_temp() {
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
fn real_launcher_cancel_kills_background_process_tree() {
    let Some(fixture) = sandbox_fixture() else {
        return;
    };
    let pid_file = fixture.workspace.join("background.pid");
    let request = shell_request(
        "sleep 120 & child=$!; printf '%s' \"$child\" > background.pid; wait".to_string(),
        &fixture.workspace,
    );
    let worker_connection = Arc::clone(&fixture.connection);
    let context = invocation_context(&fixture.workspace, "cancel-background");
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = worker_connection.invoke_with_context(RUN_SHELL_OPERATION, &request, &context);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(&pid_file, Duration::from_secs(10));
    assert!(process_exists(background_pid));
    fixture.connection.cancel_session("session-a").unwrap();
    let _ = finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("取消会话后 command 执行线程未及时结束");
    worker.join().unwrap();
    wait_for_process_exit(background_pid, Duration::from_secs(5));
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
    std::fs::create_dir_all(&workspace).unwrap();
    let connection = Arc::new(StdioSidecarConnection::new(
        SidecarConfig::new(
            "command",
            "0.0.0",
            binary,
            root.path().join("endpoint.json"),
            root.path().join("sidecar.log"),
            root.path().join("data"),
            root.path().join("storage"),
        )
        .with_timeouts(Duration::from_secs(15), Duration::from_secs(30)),
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
    let request = shell_request(
        "sleep 120 & child=$!; printf '%s' \"$child\" > background.pid; wait".to_string(),
        &workspace,
    );
    #[cfg(windows)]
    let request = windows_background_process_request(&workspace);
    let worker_connection = Arc::clone(&connection);
    let (finished_tx, finished_rx) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = worker_connection.invoke(RUN_SHELL_OPERATION, &request);
        let _ = finished_tx.send(result);
    });

    let background_pid = wait_for_pid(&pid_file, Duration::from_secs(10));
    assert!(process_exists(background_pid));
    connection.stop().unwrap();
    let _ = finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停止 sidecar 后执行线程未及时结束");
    worker.join().unwrap();
    wait_for_process_exit(background_pid, Duration::from_secs(5));
}

#[cfg(windows)]
fn windows_background_process_request(workspace: &Path) -> String {
    serde_json::json!({
        "script": concat!(
            "$child = Start-Process ",
            "-FilePath \"$env:SystemRoot\\System32\\ping.exe\" ",
            "-ArgumentList \"-t\",\"127.0.0.1\" -PassThru; ",
            "Set-Content -LiteralPath \"background.pid\" ",
            "-Value $child.Id -NoNewline -Encoding ascii; ",
            "Wait-Process -Id $child.Id"
        ),
        "shell": "powershell",
        "timeout_secs": 120,
        "workspace": workspace.display().to_string(),
        "full_trust": true,
        "allowed_commands": [],
    })
    .to_string()
}

#[cfg(unix)]
fn wait_for_pid(path: &Path, timeout: Duration) -> i32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<i32>()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "等待后台进程 PID 超时");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn wait_for_pid(path: &Path, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "等待后台进程 PID 超时");
        std::thread::sleep(Duration::from_millis(50));
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
