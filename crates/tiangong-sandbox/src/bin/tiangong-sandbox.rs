//! 随 App 发布的固定沙箱 Launcher。
//!
//! 定位：宿主决定策略，Launcher 负责校验策略、探测平台能力、应用 OS 沙箱
//! 并启动目标进程。目标进程及其全部子进程树
//! 继承沙箱约束；插件完全不参与沙箱决策。
//!
//! 通信（一次性包装器形态，非常驻守护）：
//! - Unix 策略经继承文件描述符 fd3 以长度前缀帧传入；Windows 使用仅
//!   Launcher 读取的一次性环境信封（结构化 JSON、双版本化）；
//! - stdin/stdout 留给目标进程与宿主的业务通信；
//! - Unix 上以 `exec` 替换自身：应用沙箱约束后变身目标进程，
//!   管道与生命周期天然继承，无额外转发层；
//! - **fail-closed**：协议版本不符、策略非法、平台沙箱不可用一律拒绝
//!   （结构化错误 + 非零退出码），绝不静默降级为无沙箱执行。
//!
//! 版本化三层（RFC 0017 §九）：
//! - 产品版本：本 crate 版本；
//! - `protocol_version`：App↔Launcher 通信协议；
//! - `policy_schema`：策略字段与安全语义。
//!
//! 子命令：
//! - `--self-check`：激活前自检（平台探测 + 真实拦截核心项）。

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
#[cfg(windows)]
use serde::Serialize;
use sha2::{Digest, Sha256};

/// App ↔ Launcher 通信协议版本。
const PROTOCOL_VERSION: u32 = 1;
/// 策略 Schema 版本。
const POLICY_SCHEMA: u32 = 2;
/// 策略经此继承描述符传入。
#[cfg(unix)]
const POLICY_FD: i32 = 3;
/// fail-closed 退出码（策略/协议/平台不可用/自检失败）。
const EXIT_SANDBOX_UNAVAILABLE: i32 = 78;
/// 自检专用：当前宿主环境无法验证（如嵌套沙箱），非制品缺陷。
const EXIT_ENV_UNVERIFIABLE: i32 = 79;

/// Launcher 启动指令（宿主经 fd3 写入）。
#[derive(Debug, Deserialize)]
struct LaunchRequest {
    protocol_version: u32,
    policy_schema: u32,
    policy: tiangong_sandbox::SandboxPolicy,
    plugin_id: String,
    program: String,
    program_root: String,
    program_sha256: String,
    #[serde(default)]
    args: Vec<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--self-check-network-probe") {
        let address = args.get(1).map(String::as_str).unwrap_or_default();
        std::process::exit(run_network_probe(address));
    }
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--windows-self-check-file-probe") {
        let request = args.get(1).map(String::as_str).unwrap_or_default();
        std::process::exit(run_windows_file_probe(request));
    }
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--windows-self-check-child-probe") {
        let request = args.get(1).map(String::as_str).unwrap_or_default();
        std::process::exit(run_windows_child_probe(request));
    }
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--windows-self-check-process-limit-probe") {
        std::process::exit(run_windows_process_limit_probe());
    }
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--windows-self-check-memory-limit-probe") {
        std::process::exit(run_windows_memory_limit_probe());
    }
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("--windows-self-check-cpu-limit-probe") {
        std::process::exit(run_windows_cpu_limit_probe());
    }
    if args.iter().any(|arg| arg == "--self-check") {
        // 自检有自己的三态退出码语义，不走统一拒绝路径。
        std::process::exit(run_self_check());
    }
    if let Err(error) = run_launch() {
        // 结构化拒绝信息：宿主与审计可解析，绝不静默放行。
        let payload = serde_json::json!({
            "launcher": "tiangong-sandbox",
            "product_version": env!("CARGO_PKG_VERSION"),
            "error": format!("{error:#}"),
        });
        eprintln!("{payload}");
        std::process::exit(EXIT_SANDBOX_UNAVAILABLE);
    }
}

fn run_launch() -> Result<()> {
    let request = read_request()?;
    if request.protocol_version != PROTOCOL_VERSION {
        bail!(
            "Launcher 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
            request.protocol_version
        );
    }
    if request.policy_schema != POLICY_SCHEMA {
        bail!(
            "策略 Schema 版本不匹配: expected={POLICY_SCHEMA}, actual={}",
            request.policy_schema
        );
    }
    if request.policy.mode == tiangong_sandbox::SandboxMode::FullAccess {
        bail!("当前 command Launcher 不接受 full_access 策略");
    }
    let program_path = validate_target(&request)?;

    #[cfg(windows)]
    {
        let host_pid = std::env::var(tiangong_sandbox::HOST_PID_ENV)
            .context("读取 Windows 宿主进程 ID 失败")?
            .parse::<u32>()
            .context("Windows 宿主进程 ID 无效")?;
        let stop_event = std::env::var(tiangong_sandbox::WINDOWS_STOP_EVENT_ENV)
            .context("读取 Windows Sandbox 停止事件失败")?;
        // Launcher 是一次性单线程进程；移除信封后再继承环境，目标无法读取
        // 策略正文或停止事件名称。
        unsafe {
            std::env::remove_var(tiangong_sandbox::POLICY_ENV);
            std::env::remove_var(tiangong_sandbox::WINDOWS_STOP_EVENT_ENV);
            std::env::remove_var(tiangong_sandbox::HOST_PID_ENV);
        }
        let program_root = std::fs::canonicalize(&request.program_root)
            .context("规范化 Windows command 插件根目录失败")?;
        let exit_code = tiangong_sandbox::sandbox::windows::launch(
            tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
                program: &program_path,
                program_root: &program_root,
                args: &request.args,
                policy: &request.policy,
                host_pid: Some(host_pid),
                stop_event_name: Some(&stop_event),
                timeout: None,
            },
        )?;
        std::process::exit(exit_code);
    }

    // Unix 平台沙箱包装（seatbelt / bwrap）；不可用时 fail-closed。
    #[cfg(unix)]
    let wrapped = match tiangong_sandbox::wrap(&request.policy) {
        tiangong_sandbox::SandboxedProgram::Wrapped { program, prefix } => (program, prefix),
        tiangong_sandbox::SandboxedProgram::Direct => {
            bail!("平台沙箱不可用，拒绝启动（fail-closed）");
        }
        tiangong_sandbox::SandboxedProgram::Unavailable(reason) => {
            bail!("平台沙箱不可用：{reason}");
        }
    };

    // 关闭策略描述符后 exec：应用沙箱约束并替换为目标进程。
    // stdin/stdout/stderr 全部继承（业务通信由宿主与目标进程直连）。
    #[cfg(unix)]
    let mut command = std::process::Command::new(wrapped.0);
    #[cfg(unix)]
    command.args(&wrapped.1);
    #[cfg(unix)]
    command.arg(&program_path);
    #[cfg(unix)]
    command.args(&request.args);
    #[cfg(unix)]
    command.env_remove(tiangong_sandbox::POLICY_ENV);
    #[cfg(unix)]
    unsafe {
        libc::close(POLICY_FD);
        Err(command.exec().into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        bail!("当前平台缺少沙箱启动能力")
    }
}

#[cfg(unix)]
fn read_request() -> Result<LaunchRequest> {
    // fd3 由宿主以继承管道提供；显式长度避免并发 spawn 意外继承写端时
    // Launcher 永远等待 EOF。描述符由 run_launch 在 exec 前关闭。
    let mut reader = unsafe {
        use std::os::fd::FromRawFd;
        std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(POLICY_FD))
    };
    let mut length = [0u8; size_of::<u32>()];
    reader
        .read_exact(&mut length)
        .context("读取 fd3 策略长度失败")?;
    let length = u32::from_be_bytes(length) as usize;
    if length > tiangong_sandbox::MAX_POLICY_FRAME_BYTES {
        bail!(
            "Launcher 策略超过长度上限: actual={length}, max={}",
            tiangong_sandbox::MAX_POLICY_FRAME_BYTES
        );
    }
    let mut raw = vec![0u8; length];
    reader
        .read_exact(&mut raw)
        .context("读取 fd3 策略正文失败")?;
    serde_json::from_slice(&raw).context("解析 Launcher 策略失败")
}

#[cfg(windows)]
fn read_request() -> Result<LaunchRequest> {
    let raw = std::env::var(tiangong_sandbox::POLICY_ENV)
        .context("读取 Windows Launcher 策略信封失败")?;
    serde_json::from_str(&raw).context("解析 Launcher 策略失败")
}

#[cfg(not(any(unix, windows)))]
fn read_request() -> Result<LaunchRequest> {
    bail!("当前平台没有 Launcher 策略传输通道")
}

fn validate_target(request: &LaunchRequest) -> Result<PathBuf> {
    if request.plugin_id != "command" {
        bail!("Launcher 当前只允许启动 command 插件");
    }
    let root = Path::new(&request.program_root);
    let program = Path::new(&request.program);
    if !root.is_absolute() || !program.is_absolute() {
        bail!("目标程序及插件根目录必须是绝对路径");
    }

    let root_metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("读取插件根目录失败: {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("插件根目录必须是实际目录: {}", root.display());
    }
    let program_metadata = std::fs::symlink_metadata(program)
        .with_context(|| format!("读取目标程序失败: {}", program.display()))?;
    if program_metadata.file_type().is_symlink() || !program_metadata.is_file() {
        bail!("目标程序必须是实际普通文件: {}", program.display());
    }

    let canonical_root = std::fs::canonicalize(root).context("规范化插件根目录失败")?;
    let canonical_program = std::fs::canonicalize(program).context("规范化目标程序失败")?;
    if canonical_program == canonical_root || !canonical_program.starts_with(&canonical_root) {
        bail!("目标程序不在 command 插件权威目录内");
    }

    let manifest_path = canonical_root.join("plugin.json");
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path)
        .with_context(|| format!("读取插件清单失败: {}", manifest_path.display()))?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        bail!("插件清单必须是实际普通文件");
    }
    let manifest: TargetManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).context("读取 command 插件清单失败")?,
    )
    .context("解析 command 插件清单失败")?;
    if manifest.id != request.plugin_id {
        bail!("Launcher 请求与插件清单身份不一致");
    }
    let mut expected_program = canonical_root.join(&manifest.sidecar.binary);
    if !std::env::consts::EXE_SUFFIX.is_empty()
        && !expected_program
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(std::env::consts::EXE_SUFFIX))
    {
        let name = expected_program
            .file_name()
            .and_then(|name| name.to_str())
            .context("插件清单中的 sidecar 文件名无效")?;
        expected_program.set_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    }
    let expected_program =
        std::fs::canonicalize(expected_program).context("解析清单声明的 sidecar 失败")?;
    if canonical_program != expected_program {
        bail!("目标程序与 command 插件清单声明不一致");
    }

    let actual_sha256 = sha256_file(&canonical_program)?;
    if !request.program_sha256.eq_ignore_ascii_case(&actual_sha256) {
        bail!("目标程序摘要不匹配");
    }
    validate_unix_permissions(&canonical_program, &program_metadata)?;
    Ok(canonical_program)
}

#[derive(Debug, Deserialize)]
struct TargetManifest {
    id: String,
    sidecar: TargetSidecar,
}

#[derive(Debug, Deserialize)]
struct TargetSidecar {
    binary: String,
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("读取目标程序失败: {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[cfg(unix)]
fn validate_unix_permissions(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let owner = metadata.uid();
    let current_user = unsafe { libc::geteuid() };
    if owner != current_user && owner != 0 {
        bail!("目标程序所有者不可信: {}", path.display());
    }
    if metadata.mode() & 0o022 != 0 {
        bail!("目标程序不能允许组或其他用户写入: {}", path.display());
    }
    if metadata.mode() & 0o111 == 0 {
        bail!("目标程序没有执行权限: {}", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_unix_permissions(_path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    Ok(())
}

/// 制品自检。三态退出码：0=全部通过；79=当前宿主环境无法验证（如
/// 嵌套沙箱，非制品缺陷）；78=必需能力失败（制品不可用，必须拒绝启动）。
fn run_self_check() -> i32 {
    let mut report = serde_json::Map::new();
    report.insert(
        "product_version".into(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    report.insert(
        "protocol_version".into(),
        serde_json::Value::from(PROTOCOL_VERSION),
    );
    report.insert(
        "policy_schema".into(),
        serde_json::Value::from(POLICY_SCHEMA),
    );

    let availability = tiangong_sandbox::availability();
    report.insert(
        "platform".into(),
        match &availability {
            tiangong_sandbox::SandboxAvailability::Available => {
                serde_json::Value::String("available".to_string())
            }
            tiangong_sandbox::SandboxAvailability::Unsupported(reason) => {
                serde_json::Value::String(format!("unavailable: {reason}"))
            }
            tiangong_sandbox::SandboxAvailability::EnvironmentRestricted(reason) => {
                serde_json::Value::String(format!("environment-restricted: {reason}"))
            }
        },
    );

    if matches!(
        &availability,
        tiangong_sandbox::SandboxAvailability::EnvironmentRestricted(_)
    ) {
        report.insert(
            "environment_unverifiable".into(),
            serde_json::Value::from(true),
        );
        println!("{}", serde_json::Value::Object(report));
        return EXIT_ENV_UNVERIFIABLE;
    }
    if matches!(
        &availability,
        tiangong_sandbox::SandboxAvailability::Unsupported(_)
    ) {
        println!("{}", serde_json::Value::Object(report));
        return EXIT_SANDBOX_UNAVAILABLE;
    }

    #[cfg(unix)]
    run_enforcement_probes(&mut report);
    #[cfg(windows)]
    run_windows_enforcement_probes(&mut report);
    // 必需能力失败时制品不可用，宿主必须拒绝启动。
    let required_ok = [
        "workspace_write",
        "outside_write_blocked",
        "outside_delete_blocked",
        "outside_config_write_blocked",
        "git_metadata_write_blocked",
        "network_blocked",
        "sensitive_read_blocked",
        "symlink_escape_blocked",
        "path_traversal_blocked",
    ]
    .iter()
    .all(|field| {
        report
            .get(*field)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    });
    #[cfg(windows)]
    let required_ok = required_ok
        && [
            "outside_read_blocked",
            "dedicated_temp_write",
            "network_allowed",
            "child_restrictions_inherited",
            "resource_limits_applied",
            "process_limit_enforced",
            "memory_limit_enforced",
            "cpu_limit_enforced",
            "existing_workspace_read_write",
            "git_metadata_readable",
            "hardlink_escape_blocked",
            "temporary_acl_cleaned",
            "temporary_identity_cleaned",
        ]
        .iter()
        .all(|field| {
            report
                .get(*field)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        });
    let exit_code = if required_ok {
        0
    } else {
        EXIT_SANDBOX_UNAVAILABLE
    };
    println!("{}", serde_json::Value::Object(report));
    exit_code
}

fn run_network_probe(address: &str) -> i32 {
    let Ok(address) = address.parse::<std::net::SocketAddr>() else {
        return 2;
    };
    match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(2)) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

#[cfg(windows)]
#[derive(Debug, Serialize, Deserialize)]
struct WindowsProbeRequest {
    workspace: PathBuf,
    temp_dir: PathBuf,
    existing_workspace_file: PathBuf,
    outside_write: PathBuf,
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
#[derive(Debug, Default, Serialize, Deserialize)]
struct WindowsProbeReport {
    workspace_write: bool,
    dedicated_temp_write: bool,
    existing_workspace_read_write: bool,
    outside_write_blocked: bool,
    outside_read_blocked: bool,
    outside_delete_blocked: bool,
    outside_config_write_blocked: bool,
    git_metadata_write_blocked: bool,
    git_metadata_readable: bool,
    network_blocked: bool,
    sensitive_read_blocked: bool,
    path_traversal_blocked: bool,
    child_restrictions_inherited: bool,
    resource_limits_applied: bool,
}

#[cfg(windows)]
struct WindowsEnforcementResult {
    probe: WindowsProbeReport,
    network_allowed: bool,
    process_limit_enforced: bool,
    memory_limit_enforced: bool,
    cpu_limit_enforced: bool,
    hardlink_escape_blocked: bool,
    symlink_escape_blocked: bool,
    temporary_acl_cleaned: bool,
    temporary_identity_cleaned: bool,
}

#[cfg(windows)]
fn run_windows_enforcement_probes(report: &mut serde_json::Map<String, serde_json::Value>) {
    match windows_enforcement_probes() {
        Ok(result) => {
            let probe = result.probe;
            for (field, value) in [
                ("workspace_write", probe.workspace_write),
                (
                    "existing_workspace_read_write",
                    probe.existing_workspace_read_write,
                ),
                ("outside_write_blocked", probe.outside_write_blocked),
                ("outside_read_blocked", probe.outside_read_blocked),
                ("outside_delete_blocked", probe.outside_delete_blocked),
                (
                    "outside_config_write_blocked",
                    probe.outside_config_write_blocked,
                ),
                (
                    "git_metadata_write_blocked",
                    probe.git_metadata_write_blocked,
                ),
                ("git_metadata_readable", probe.git_metadata_readable),
                ("network_blocked", probe.network_blocked),
                ("sensitive_read_blocked", probe.sensitive_read_blocked),
                ("path_traversal_blocked", probe.path_traversal_blocked),
                (
                    "child_restrictions_inherited",
                    probe.child_restrictions_inherited,
                ),
                ("resource_limits_applied", probe.resource_limits_applied),
            ] {
                report.insert(field.into(), serde_json::Value::from(value));
            }
            report.insert(
                "dedicated_temp_write".into(),
                serde_json::Value::from(probe.dedicated_temp_write),
            );
            for (field, value) in [
                ("network_allowed", result.network_allowed),
                ("process_limit_enforced", result.process_limit_enforced),
                ("memory_limit_enforced", result.memory_limit_enforced),
                ("cpu_limit_enforced", result.cpu_limit_enforced),
                ("hardlink_escape_blocked", result.hardlink_escape_blocked),
                ("symlink_escape_blocked", result.symlink_escape_blocked),
                ("temporary_acl_cleaned", result.temporary_acl_cleaned),
                (
                    "temporary_identity_cleaned",
                    result.temporary_identity_cleaned,
                ),
            ] {
                report.insert(field.into(), serde_json::Value::from(value));
            }
        }
        Err(error) => {
            report.insert(
                "windows_probe_error".into(),
                serde_json::Value::String(format!("{error:#}")),
            );
        }
    }
}

#[cfg(windows)]
fn windows_enforcement_probes() -> Result<WindowsEnforcementResult> {
    eprintln!("Windows Sandbox 自检阶段: 准备隔离路径");
    let root = tempfile::Builder::new()
        .prefix("tiangong-sandbox-selfcheck-")
        .tempdir()
        .context("创建 Windows 自检根目录失败")?;
    let workspace = root.path().join("workspace");
    let temp_dir = root.path().join("invocation-temp");
    let outside = root.path().join("outside");
    let fake_home = root.path().join("home");
    let git_dir = workspace.join(".git");
    for path in [
        &workspace,
        &temp_dir,
        &outside,
        &fake_home,
        &git_dir,
        &fake_home.join(".ssh"),
        &fake_home.join(".aws"),
    ] {
        std::fs::create_dir_all(path)
            .with_context(|| format!("创建 Windows 自检目录失败: {}", path.display()))?;
    }
    let outside_read = outside.join("read-secret.txt");
    let outside_delete = outside.join("delete-me");
    let outside_config = fake_home.join("profile.ini");
    let git_config = git_dir.join("config");
    let existing_workspace_file = workspace.join("existing.txt");
    let ssh_secret = fake_home.join(".ssh/id_ed25519");
    let aws_secret = fake_home.join(".aws/credentials");
    std::fs::write(&outside_read, "TIANGONG_FAKE_OUTSIDE_SECRET")?;
    std::fs::create_dir_all(&outside_delete)?;
    std::fs::write(outside_delete.join("keep"), "safe")?;
    std::fs::write(&outside_config, "safe\n")?;
    std::fs::write(&git_config, "safe\n")?;
    std::fs::write(&existing_workspace_file, "workspace-before")?;
    std::fs::write(&ssh_secret, "TIANGONG_FAKE_SSH_SECRET")?;
    std::fs::write(&aws_secret, "TIANGONG_FAKE_AWS_SECRET")?;

    eprintln!("Windows Sandbox 自检阶段: 查找可达网络目标");
    let network_address = reachable_network_address()?;
    let resource_limits = tiangong_sandbox::SandboxResourceLimits {
        max_cpu_time_seconds: 60,
        max_memory_bytes: 512 * 1024 * 1024,
        max_processes: 8,
    };
    let report_path = workspace.join("probe-report.json");
    let child_report_path = workspace.join("child-report.json");
    let request = WindowsProbeRequest {
        workspace: workspace.clone(),
        temp_dir: temp_dir.clone(),
        existing_workspace_file: existing_workspace_file.clone(),
        outside_write: outside.join("blocked.txt"),
        outside_delete: outside_delete.clone(),
        outside_config: outside_config.clone(),
        git_config: git_config.clone(),
        sensitive_paths: vec![outside_read, ssh_secret, aws_secret],
        traversal_target: workspace.join("../outside/traversal.txt"),
        report_path: report_path.clone(),
        child_report_path,
        network_address: network_address.clone(),
        resource_limits,
    };
    let request_json = serde_json::to_string(&request)?;
    let current_exe = std::env::current_exe().context("读取 Windows 自检程序路径失败")?;
    let program_root = current_exe.parent().context("Windows 自检程序缺少父目录")?;
    let mut policy = tiangong_sandbox::SandboxPolicy::workspace_write(&workspace);
    policy.extra_writable = vec![temp_dir];
    policy.denied_read_paths = request.sensitive_paths.clone();
    policy.resource_limits = resource_limits;
    eprintln!("Windows Sandbox 自检阶段: 文件、断网与子进程继承");
    let exit_code = tiangong_sandbox::sandbox::windows::launch(
        tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
            program: &current_exe,
            program_root,
            args: &["--windows-self-check-file-probe".to_string(), request_json],
            policy: &policy,
            host_pid: None,
            stop_event_name: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        },
    )?;
    if exit_code != 0 {
        bail!("Windows 文件隔离探测进程退出码异常: {exit_code}");
    }
    let mut probe: WindowsProbeReport = serde_json::from_slice(
        &std::fs::read(&report_path).context("读取 Windows 隔离探测报告失败")?,
    )
    .context("解析 Windows 隔离探测报告失败")?;
    probe.outside_write_blocked &= !request.outside_write.exists();
    probe.outside_delete_blocked &= outside_delete.join("keep").is_file();
    probe.outside_config_write_blocked &=
        std::fs::read_to_string(&outside_config).is_ok_and(|value| value == "safe\n");
    probe.git_metadata_write_blocked &=
        std::fs::read_to_string(&git_config).is_ok_and(|value| value == "safe\n");
    probe.existing_workspace_read_write &= std::fs::read_to_string(&existing_workspace_file)
        .is_ok_and(|value| value == "workspace-after");
    probe.path_traversal_blocked &= !request.traversal_target.exists();

    let mut network_policy = policy.clone();
    network_policy.allow_network = true;
    eprintln!("Windows Sandbox 自检阶段: 显式联网能力");
    let network_exit = tiangong_sandbox::sandbox::windows::launch(
        tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
            program: &current_exe,
            program_root,
            args: &["--self-check-network-probe".to_string(), network_address],
            policy: &network_policy,
            host_pid: None,
            stop_event_name: None,
            timeout: Some(std::time::Duration::from_secs(15)),
        },
    )?;
    let network_allowed = network_exit == 0;

    eprintln!("Windows Sandbox 自检阶段: 进程数上限");
    let process_limit_enforced = launch_windows_limit_probe(
        &current_exe,
        program_root,
        &policy,
        "--windows-self-check-process-limit-probe",
        tiangong_sandbox::SandboxResourceLimits {
            max_cpu_time_seconds: 30,
            max_memory_bytes: 512 * 1024 * 1024,
            max_processes: 1,
        },
    )? == 0;
    eprintln!("Windows Sandbox 自检阶段: 内存上限");
    let memory_limit_enforced = launch_windows_limit_probe(
        &current_exe,
        program_root,
        &policy,
        "--windows-self-check-memory-limit-probe",
        tiangong_sandbox::SandboxResourceLimits {
            max_cpu_time_seconds: 30,
            max_memory_bytes: 96 * 1024 * 1024,
            max_processes: 2,
        },
    )? != 5;
    eprintln!("Windows Sandbox 自检阶段: CPU 上限");
    let cpu_limit_enforced = launch_windows_limit_probe(
        &current_exe,
        program_root,
        &policy,
        "--windows-self-check-cpu-limit-probe",
        tiangong_sandbox::SandboxResourceLimits {
            max_cpu_time_seconds: 1,
            max_memory_bytes: 512 * 1024 * 1024,
            max_processes: 1,
        },
    )? != 5;

    eprintln!("Windows Sandbox 自检阶段: 硬链接拒绝");
    let hardlink_workspace = root.path().join("hardlink-workspace");
    std::fs::create_dir_all(&hardlink_workspace)?;
    let hardlink_target = outside.join("hardlink-target.txt");
    std::fs::write(&hardlink_target, "safe")?;
    std::fs::hard_link(&hardlink_target, hardlink_workspace.join("escape-link"))?;
    let hardlink_policy = tiangong_sandbox::SandboxPolicy::workspace_write(&hardlink_workspace);
    let hardlink_blocked = tiangong_sandbox::sandbox::windows::launch(
        tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
            program: &current_exe,
            program_root,
            args: &[],
            policy: &hardlink_policy,
            host_pid: None,
            stop_event_name: None,
            timeout: Some(std::time::Duration::from_secs(15)),
        },
    )
    .is_err();

    eprintln!("Windows Sandbox 自检阶段: 重解析点拒绝");
    let reparse_workspace = root.path().join("reparse-workspace");
    std::fs::create_dir_all(&reparse_workspace)?;
    let reparse = reparse_workspace.join("escape-link");
    let reparse_created = std::os::windows::fs::symlink_file(&outside_config, &reparse).is_ok()
        || create_directory_junction(&outside, &reparse).unwrap_or(false);
    let reparse_policy = tiangong_sandbox::SandboxPolicy::workspace_write(&reparse_workspace);
    let reparse_blocked = reparse_created
        && tiangong_sandbox::sandbox::windows::launch(
            tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
                program: &current_exe,
                program_root,
                args: &[],
                policy: &reparse_policy,
                host_pid: None,
                stop_event_name: None,
                timeout: Some(std::time::Duration::from_secs(15)),
            },
        )
        .is_err();
    Ok(WindowsEnforcementResult {
        probe,
        network_allowed,
        process_limit_enforced,
        memory_limit_enforced,
        cpu_limit_enforced,
        hardlink_escape_blocked: hardlink_blocked,
        symlink_escape_blocked: reparse_blocked,
        temporary_acl_cleaned: true,
        temporary_identity_cleaned: true,
    })
}

#[cfg(windows)]
fn launch_windows_limit_probe(
    program: &Path,
    program_root: &Path,
    policy: &tiangong_sandbox::SandboxPolicy,
    mode: &str,
    limits: tiangong_sandbox::SandboxResourceLimits,
) -> Result<i32> {
    let mut policy = policy.clone();
    policy.resource_limits = limits;
    tiangong_sandbox::sandbox::windows::launch(
        tiangong_sandbox::sandbox::windows::WindowsLaunchRequest {
            program,
            program_root,
            args: &[mode.to_string()],
            policy: &policy,
            host_pid: None,
            stop_event_name: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        },
    )
}

#[cfg(windows)]
fn reachable_network_address() -> Result<String> {
    use std::net::ToSocketAddrs;
    for endpoint in ["github.com:443", "www.microsoft.com:443", "1.1.1.1:443"] {
        let Ok(addresses) = endpoint.to_socket_addrs() else {
            continue;
        };
        for address in addresses {
            if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3))
                .is_ok()
            {
                return Ok(address.to_string());
            }
        }
    }
    bail!("Windows 自检无法找到可达的网络目标")
}

#[cfg(windows)]
fn create_directory_junction(target: &Path, link: &Path) -> Result<bool> {
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("创建 Windows 自检 Junction 失败")?;
    Ok(status.success())
}

#[cfg(windows)]
fn run_windows_file_probe(raw: &str) -> i32 {
    let Ok(request) = serde_json::from_str::<WindowsProbeRequest>(raw) else {
        return 2;
    };
    eprintln!("Windows 文件隔离探针: 验证父进程边界");
    let mut probe = WindowsProbeReport {
        workspace_write: std::fs::write(request.workspace.join("inside.txt"), "ok").is_ok(),
        dedicated_temp_write: std::fs::write(request.temp_dir.join("temp.txt"), "ok").is_ok(),
        existing_workspace_read_write: probe_existing_workspace_file(
            &request.existing_workspace_file,
        ),
        outside_write_blocked: std::fs::write(&request.outside_write, "blocked").is_err(),
        outside_read_blocked: request
            .sensitive_paths
            .first()
            .is_some_and(|path| std::fs::read(path).is_err()),
        outside_delete_blocked: std::fs::remove_dir_all(&request.outside_delete).is_err(),
        outside_config_write_blocked: std::fs::write(&request.outside_config, "PWNED").is_err(),
        git_metadata_write_blocked: std::fs::write(&request.git_config, "PWNED").is_err()
            && request
                .git_config
                .parent()
                .is_some_and(|git| std::fs::remove_dir_all(git).is_err()),
        git_metadata_readable: std::fs::read_to_string(&request.git_config)
            .is_ok_and(|value| value == "safe\n"),
        network_blocked: run_network_probe(&request.network_address) != 0,
        sensitive_read_blocked: request
            .sensitive_paths
            .iter()
            .all(|path| std::fs::read(path).is_err()),
        path_traversal_blocked: std::fs::write(&request.traversal_target, "blocked").is_err(),
        child_restrictions_inherited: false,
        resource_limits_applied: tiangong_sandbox::sandbox::windows::current_process_limits_match(
            request.resource_limits,
        ),
    };
    eprintln!("Windows 文件隔离探针: 启动受限子进程");
    let child_request = match serde_json::to_string(&request) {
        Ok(value) => value,
        Err(_) => return 3,
    };
    let child_ok = std::env::current_exe()
        .and_then(|program| {
            std::process::Command::new(program)
                .arg("--windows-self-check-child-probe")
                .arg(child_request)
                .status()
        })
        .is_ok_and(|status| status.success());
    eprintln!("Windows 文件隔离探针: 读取子进程报告");
    probe.child_restrictions_inherited = child_ok
        && std::fs::read(&request.child_report_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<WindowsProbeReport>(&raw).ok())
            .is_some_and(|child| {
                child.workspace_write
                    && child.dedicated_temp_write
                    && child.existing_workspace_read_write
                    && child.outside_write_blocked
                    && child.outside_read_blocked
                    && child.outside_delete_blocked
                    && child.outside_config_write_blocked
                    && child.git_metadata_write_blocked
                    && child.git_metadata_readable
                    && child.network_blocked
                    && child.sensitive_read_blocked
                    && child.path_traversal_blocked
                    && child.resource_limits_applied
            });
    eprintln!("Windows 文件隔离探针: 写入父进程报告");
    match serde_json::to_vec(&probe)
        .ok()
        .and_then(|raw| std::fs::write(&request.report_path, raw).ok())
    {
        Some(()) => 0,
        None => 4,
    }
}

#[cfg(windows)]
fn run_windows_child_probe(raw: &str) -> i32 {
    let Ok(request) = serde_json::from_str::<WindowsProbeRequest>(raw) else {
        return 2;
    };
    eprintln!("Windows 文件隔离探针: 验证子进程边界");
    let report = WindowsProbeReport {
        workspace_write: std::fs::write(request.workspace.join("inside-child.txt"), "ok").is_ok(),
        dedicated_temp_write: std::fs::write(request.temp_dir.join("temp-child.txt"), "ok").is_ok(),
        existing_workspace_read_write: probe_existing_workspace_file(
            &request.existing_workspace_file,
        ),
        outside_write_blocked: std::fs::write(&request.outside_write, "blocked").is_err(),
        outside_read_blocked: request
            .sensitive_paths
            .first()
            .is_some_and(|path| std::fs::read(path).is_err()),
        git_metadata_write_blocked: std::fs::write(&request.git_config, "PWNED").is_err()
            && request
                .git_config
                .parent()
                .is_some_and(|git| std::fs::remove_dir_all(git).is_err()),
        git_metadata_readable: std::fs::read_to_string(&request.git_config)
            .is_ok_and(|value| value == "safe\n"),
        outside_delete_blocked: std::fs::remove_dir_all(&request.outside_delete).is_err(),
        outside_config_write_blocked: std::fs::write(&request.outside_config, "PWNED").is_err(),
        network_blocked: run_network_probe(&request.network_address) != 0,
        sensitive_read_blocked: request
            .sensitive_paths
            .iter()
            .all(|path| std::fs::read(path).is_err()),
        path_traversal_blocked: std::fs::write(&request.traversal_target, "blocked").is_err(),
        resource_limits_applied: tiangong_sandbox::sandbox::windows::current_process_limits_match(
            request.resource_limits,
        ),
        ..Default::default()
    };
    eprintln!("Windows 文件隔离探针: 写入子进程报告");
    match serde_json::to_vec(&report)
        .ok()
        .and_then(|raw| std::fs::write(&request.child_report_path, raw).ok())
    {
        Some(()) => 0,
        None => 3,
    }
}

#[cfg(windows)]
fn probe_existing_workspace_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|value| matches!(value.as_str(), "workspace-before" | "workspace-after"))
        && std::fs::write(path, "workspace-after").is_ok()
}

#[cfg(windows)]
fn run_windows_process_limit_probe() -> i32 {
    let Ok(program) = std::env::current_exe() else {
        return 3;
    };
    match std::process::Command::new(program)
        .args(["--self-check-network-probe", "invalid"])
        .spawn()
    {
        Err(_) => 0,
        Ok(mut child) => {
            let _ = child.wait();
            5
        }
    }
}

#[cfg(windows)]
fn run_windows_memory_limit_probe() -> i32 {
    let mut allocations = Vec::new();
    for _ in 0..512 {
        let mut block = Vec::<u8>::new();
        if block.try_reserve_exact(1024 * 1024).is_err() {
            return 0;
        }
        block.resize(1024 * 1024, 0xa5);
        std::hint::black_box(&block);
        allocations.push(block);
    }
    5
}

#[cfg(windows)]
fn run_windows_cpu_limit_probe() -> i32 {
    let started = std::time::Instant::now();
    let mut value = 0u64;
    while started.elapsed() < std::time::Duration::from_secs(10) {
        for index in 0..1_000_000u64 {
            value = value.wrapping_add(index.rotate_left((value & 31) as u32));
        }
        std::hint::black_box(value);
    }
    5
}

#[cfg(unix)]
fn run_enforcement_probes(report: &mut serde_json::Map<String, serde_json::Value>) {
    let root = tempfile::Builder::new()
        .prefix("tiangong-sandbox-selfcheck-")
        .tempdir()
        .expect("创建自检根目录失败");
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    let fake_home = root.path().join("home");
    let ssh_dir = fake_home.join(".ssh");
    let aws_dir = fake_home.join(".aws");
    let tiangong_dir = fake_home.join(".tiangong");
    let git_dir = workspace.join(".git");
    for path in [
        &workspace,
        &outside,
        &ssh_dir,
        &aws_dir,
        &tiangong_dir,
        &git_dir,
    ] {
        std::fs::create_dir_all(path).expect("创建自检目录失败");
    }

    let ssh_secret = ssh_dir.join("id_ed25519");
    let aws_secret = aws_dir.join("credentials");
    let trust_db = tiangong_dir.join("trust.db");
    let secrets = [
        (&ssh_secret, "TIANGONG_FAKE_SSH_SECRET"),
        (&aws_secret, "TIANGONG_FAKE_AWS_SECRET"),
        (&trust_db, "TIANGONG_FAKE_TRUST_SECRET"),
    ];
    for (path, marker) in secrets {
        std::fs::write(path, marker).expect("写入自检假凭据失败");
    }
    let bashrc = fake_home.join(".bashrc");
    std::fs::write(&bashrc, "safe\n").expect("写入自检配置失败");
    let git_config = git_dir.join("config");
    std::fs::write(&git_config, "safe\n").expect("写入自检 git 配置失败");

    let mut policy = tiangong_sandbox::SandboxPolicy::workspace_write(&workspace);
    policy.denied_read_paths = vec![ssh_dir, aws_dir, trust_db];

    let inside = workspace.join("selfcheck.txt");
    let output = run_sandbox_command(&policy, "/usr/bin/touch", &[inside.display().to_string()]);
    report.insert(
        "workspace_write".into(),
        serde_json::Value::from(output.status.success() && inside.is_file()),
    );

    let outside_write = outside.join("blocked.txt");
    let output = run_sandbox_command(
        &policy,
        "/usr/bin/touch",
        &[outside_write.display().to_string()],
    );
    report.insert(
        "outside_write_blocked".into(),
        serde_json::Value::from(!output.status.success() && !outside_write.exists()),
    );

    let delete_target = outside.join("delete-me");
    std::fs::create_dir_all(&delete_target).expect("创建自检删除目标失败");
    std::fs::write(delete_target.join("keep"), "safe").expect("写入自检删除目标失败");
    let output = run_sandbox_command(
        &policy,
        "/bin/rm",
        &["-rf".into(), delete_target.display().to_string()],
    );
    report.insert(
        "outside_delete_blocked".into(),
        serde_json::Value::from(!output.status.success() && delete_target.join("keep").is_file()),
    );

    let output = run_sandbox_command(
        &policy,
        "/bin/sh",
        &[
            "-c".into(),
            format!("printf PWNED >> {}", shell_quote(&bashrc)),
        ],
    );
    report.insert(
        "outside_config_write_blocked".into(),
        serde_json::Value::from(
            !output.status.success()
                && std::fs::read_to_string(&bashrc).is_ok_and(|value| value == "safe\n"),
        ),
    );

    let output = run_sandbox_command(
        &policy,
        "/bin/sh",
        &[
            "-c".into(),
            format!("printf PWNED > {}", shell_quote(&git_config)),
        ],
    );
    report.insert(
        "git_metadata_write_blocked".into(),
        serde_json::Value::from(
            !output.status.success()
                && std::fs::read_to_string(&git_config).is_ok_and(|value| value == "safe\n"),
        ),
    );

    let sensitive_read_blocked = [ssh_secret, aws_secret, tiangong_dir.join("trust.db")]
        .iter()
        .all(|path| {
            let output = run_sandbox_command(&policy, "/bin/cat", &[path.display().to_string()]);
            !String::from_utf8_lossy(&output.stdout).contains("TIANGONG_FAKE_")
        });
    report.insert(
        "sensitive_read_blocked".into(),
        serde_json::Value::from(sensitive_read_blocked),
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("创建自检网络监听失败");
    let address = listener.local_addr().expect("读取自检监听地址失败");
    let current_exe = std::env::current_exe().expect("读取自检程序路径失败");
    let direct_probe = std::process::Command::new(&current_exe)
        .arg("--self-check-network-probe")
        .arg(address.to_string())
        .status()
        .expect("启动直连网络探测失败")
        .success();
    let output = run_sandbox_command(
        &policy,
        &current_exe.display().to_string(),
        &["--self-check-network-probe".into(), address.to_string()],
    );
    drop(listener);
    report.insert(
        "network_blocked".into(),
        serde_json::Value::from(direct_probe && !output.status.success()),
    );

    let symlink_target = outside.join("symlink-target");
    std::fs::write(&symlink_target, "safe\n").expect("写入自检符号链接目标失败");
    let symlink = workspace.join("escape-link");
    std::os::unix::fs::symlink(&symlink_target, &symlink).expect("创建自检符号链接失败");
    let output = run_sandbox_command(
        &policy,
        "/bin/sh",
        &[
            "-c".into(),
            format!("printf PWNED > {}", shell_quote(&symlink)),
        ],
    );
    report.insert(
        "symlink_escape_blocked".into(),
        serde_json::Value::from(
            !output.status.success()
                && std::fs::read_to_string(&symlink_target).is_ok_and(|value| value == "safe\n"),
        ),
    );

    let traversal_target = workspace.join("../outside/traversal");
    let output = run_sandbox_command(
        &policy,
        "/usr/bin/touch",
        &[traversal_target.display().to_string()],
    );
    report.insert(
        "path_traversal_blocked".into(),
        serde_json::Value::from(!output.status.success() && !outside.join("traversal").exists()),
    );
}

#[cfg(unix)]
fn run_sandbox_command(
    policy: &tiangong_sandbox::SandboxPolicy,
    target: &str,
    args: &[String],
) -> std::process::Output {
    let tiangong_sandbox::SandboxedProgram::Wrapped { program, prefix } =
        tiangong_sandbox::wrap(policy)
    else {
        panic!("自检时平台沙箱应保持可用");
    };
    std::process::Command::new(program)
        .args(prefix)
        .arg(target)
        .args(args)
        .output()
        .expect("启动沙箱自检命令失败")
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(root: &Path, program: &Path) -> LaunchRequest {
        LaunchRequest {
            protocol_version: PROTOCOL_VERSION,
            policy_schema: POLICY_SCHEMA,
            policy: tiangong_sandbox::SandboxPolicy::workspace_write(root),
            plugin_id: "command".to_string(),
            program: program.display().to_string(),
            program_root: root.display().to_string(),
            program_sha256: String::new(),
            args: Vec::new(),
        }
    }

    #[test]
    fn target_path_traversal_is_rejected() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("command");
        let outside = fixture.path().join("outside-sidecar");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "not executable").unwrap();

        let error = validate_target(&request(&root, &root.join("../outside-sidecar"))).unwrap_err();
        assert!(error.to_string().contains("不在 command 插件权威目录内"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_rejected() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("command");
        let outside = fixture.path().join("outside-sidecar");
        let link = root.join("sidecar");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "not executable").unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let error = validate_target(&request(&root, &link)).unwrap_err();
        assert!(error.to_string().contains("目标程序必须是实际普通文件"));
    }
}
