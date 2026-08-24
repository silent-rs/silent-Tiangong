//! 官方沙箱 Launcher（RFC 0017 独立版本化安全组件，第一阶段）。
//!
//! 定位：天工决策（策略、审批、版本选择），Launcher 可靠实施——校验策略、
//! 探测平台能力、应用 OS 沙箱、启动目标进程。目标进程及其全部子进程树
//! 继承沙箱约束；插件完全不参与沙箱决策。
//!
//! 通信（一次性包装器形态，非常驻守护）：
//! - 策略经**继承文件描述符 fd3** 传入（结构化 JSON、双版本化），
//!   stdin/stdout 留给目标进程与宿主的业务通信（exec 后透传）；
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

use std::io::Read;
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// App ↔ Launcher 通信协议版本。
const PROTOCOL_VERSION: u32 = 1;
/// 策略 Schema 版本。
const POLICY_SCHEMA: u32 = 1;
/// 策略经此继承描述符传入。
const POLICY_FD: i32 = 3;
/// fail-closed 退出码（策略/协议/平台不可用）。
const EXIT_SANDBOX_UNAVAILABLE: i32 = 78;

/// Launcher 启动指令（宿主经 fd3 写入）。
#[derive(Debug, Deserialize)]
struct LaunchRequest {
    protocol_version: u32,
    policy_schema: u32,
    policy: tiangong_sandbox::SandboxPolicy,
    program: String,
    #[serde(default)]
    args: Vec<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if args.iter().any(|arg| arg == "--self-check") {
        run_self_check()
    } else {
        run_launch()
    };
    if let Err(error) = result {
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
        bail!("Launcher 拒绝 full_access 策略（无沙箱执行走宿主独立高危通道）");
    }
    if !std::path::Path::new(&request.program).is_file() {
        bail!("目标程序不存在: {}", request.program);
    }

    // 平台沙箱包装（seatbelt / bwrap）；不可用时 fail-closed。
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
    let mut command = std::process::Command::new(wrapped.0);
    command.args(&wrapped.1);
    command.arg(&request.program);
    command.args(&request.args);
    unsafe {
        libc::close(POLICY_FD);
        Err(command.exec().into())
    }
}

fn read_request() -> Result<LaunchRequest> {
    let mut raw = String::new();
    // fd3 由宿主以继承管道提供；读取到 EOF 后由 run_launch 关闭再 exec。
    let mut reader = unsafe {
        use std::os::fd::FromRawFd;
        std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(POLICY_FD))
    };
    reader
        .read_to_string(&mut raw)
        .context("读取 fd3 策略失败")?;
    serde_json::from_str(&raw).context("解析 Launcher 策略失败")
}

/// 激活前自检（核心项；宿主环境已在沙箱内的项自动跳过并标注）。
fn run_self_check() -> Result<()> {
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
        },
    );

    if let tiangong_sandbox::SandboxAvailability::Available = availability {
        // 真实拦截核心项：工作区内写成功、工作区外写被拒。
        let workspace = tempfile::tempdir().expect("创建自检工作区失败");
        let policy = tiangong_sandbox::SandboxPolicy::workspace_write(workspace.path());
        if let tiangong_sandbox::SandboxedProgram::Wrapped { program, prefix } =
            tiangong_sandbox::wrap(&policy)
        {
            let inside = workspace.path().join("selfcheck.txt");
            let status = std::process::Command::new(&program)
                .args(&prefix)
                .arg("/bin/bash")
                .arg("-c")
                .arg(format!("echo ok > {}", inside.display()))
                .status()
                .expect("自检命令启动失败");
            let inside_ok = status.success() && inside.is_file();
            report.insert("workspace_write".into(), serde_json::Value::from(inside_ok));

            let outside = std::env::temp_dir().join(format!(
                "tiangong-launcher-selfcheck-{}",
                std::process::id()
            ));
            let outside_parent = outside.clone();
            let status = std::process::Command::new(&program)
                .args(&prefix)
                .arg("/bin/bash")
                .arg("-c")
                .arg(format!("echo x > {}/blocked.txt", outside_parent.display()))
                .status()
                .expect("自检命令启动失败");
            let blocked = !status.success() || !outside.join("blocked.txt").is_file();
            let _ = std::fs::remove_dir_all(&outside);
            report.insert(
                "outside_write_blocked".into(),
                serde_json::Value::from(blocked),
            );
        }
    }
    println!("{}", serde_json::Value::Object(report));
    Ok(())
}
