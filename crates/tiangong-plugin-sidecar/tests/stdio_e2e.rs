//! stdio 传输端到端测试（RFC 0017 S2）。
//!
//! 真实 spawn 测试用 echo sidecar 二进制，验证：
//! Auth 首帧 → 握手 → 请求往返 → 进程崩溃后自动换代重启。

use std::path::PathBuf;
use std::time::Duration;

use tiangong_plugin_runtime::protocol::PROTOCOL_VERSION;
use tiangong_plugin_runtime::sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection};

fn echo_sidecar_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_test-stdio-sidecar"))
}

#[cfg(target_os = "macos")]
fn test_host_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_test-stdio-host"))
}

fn temp_paths(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("tiangong-stdio-e2e-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    (
        base.join("endpoint.json"),
        base.join("sidecar.log"),
        base.join("data"),
    )
}

fn connection(tag: &str) -> StdioSidecarConnection {
    let (endpoint, log, data_dir) = temp_paths(tag);
    let config = SidecarConfig::new(
        "test-stdio",
        "0.0.0",
        echo_sidecar_binary(),
        endpoint,
        log,
        data_dir,
        std::env::temp_dir(),
    )
    .with_timeouts(Duration::from_secs(10), Duration::from_secs(10));
    StdioSidecarConnection::new(config)
}

#[test]
fn stdio_roundtrip_and_restart() {
    let connection = connection("roundtrip");

    // 请求往返：echo 原样回显。
    let payload = serde_json::json!({"text": "hello stdio"});
    let raw = connection.invoke("echo", &payload.to_string()).unwrap();
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["text"], "hello stdio");

    // crash 使子进程退出；下一次请求应自动换代重启。
    let _ = connection.invoke("crash", "{}");
    std::thread::sleep(Duration::from_millis(200));
    let raw = connection.invoke("echo", r#"{"after":"restart"}"#).unwrap();
    let response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(response["after"], "restart");

    connection.stop().unwrap();
}

#[test]
fn stdio_handshake_reports_identity() {
    let connection = connection("handshake");
    // 握手经 runtime.handshake 完成（ensure_running 内部调用）；
    // 随后正常请求可用即证明身份校验通过。
    let raw = connection.invoke("echo", r#"{"probe":1}"#).unwrap();
    assert!(raw.contains("probe"));

    // 错误操作经 Response 错误信封返回（与 TCP 语义一致）。
    let error = connection.invoke("no-such-op", "{}").unwrap_err();
    assert!(error.to_string().contains("未知操作"));
    let _ = PROTOCOL_VERSION;
    connection.stop().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn macos_host_crash_kills_busy_sidecar_process_group() {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let base =
        std::env::temp_dir().join(format!("tiangong-stdio-host-crash-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let child = Command::new(test_host_binary())
        .arg(echo_sidecar_binary())
        .arg(&base)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = MacProcessGuard {
        host: child,
        sidecar_pid: None,
    };

    let sidecar_pid = wait_for_pid_file(&base.join("sidecar.pid"), Duration::from_secs(10));
    let child_pid = wait_for_pid_file(&base.join("child.pid"), Duration::from_secs(10));
    guard.sidecar_pid = Some(sidecar_pid);
    assert!(process_alive(sidecar_pid));
    assert!(process_alive(child_pid));

    unsafe { libc::kill(guard.host.id() as libc::pid_t, libc::SIGKILL) };
    let _ = guard.host.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (process_alive(sidecar_pid) || process_alive(child_pid)) {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!process_alive(sidecar_pid), "宿主退出后 sidecar 仍存活");
    assert!(!process_alive(child_pid), "宿主退出后后台子进程仍存活");
    guard.sidecar_pid = None;
}

#[cfg(target_os = "macos")]
struct MacProcessGuard {
    host: std::process::Child,
    sidecar_pid: Option<libc::pid_t>,
}

#[cfg(target_os = "macos")]
impl Drop for MacProcessGuard {
    fn drop(&mut self) {
        unsafe { libc::kill(self.host.id() as libc::pid_t, libc::SIGKILL) };
        let _ = self.host.wait();
        if let Some(pid) = self.sidecar_pid {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_for_pid_file(path: &std::path::Path, timeout: Duration) -> libc::pid_t {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
        {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("等待 PID 文件超时: {}", path.display());
}

#[cfg(target_os = "macos")]
fn process_alive(pid: libc::pid_t) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
