//! 测试用 stdio 宿主：启动 sidecar 并保持一个长请求运行。
//!
//! 宿主崩溃清理测试（stdio_e2e）以 SIGKILL 终止本进程，验证 sidecar
//! 进程组随宿主退出被清理。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tiangong_plugin_runtime::sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let sidecar = PathBuf::from(args.next().context("缺少 sidecar 路径")?);
    let state_dir = args.next().map(PathBuf::from).context("缺少状态目录")?;
    if args.next().is_some() {
        bail!("参数过多");
    }
    run_stdio(sidecar, state_dir)
}

fn run_stdio(sidecar: PathBuf, state_dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&state_dir).context("创建测试状态目录失败")?;
    let config = SidecarConfig::new(
        "test-stdio",
        "0.0.0",
        sidecar,
        state_dir.join("endpoint.json"),
        state_dir.join("sidecar.log"),
        state_dir.join("data"),
        state_dir.join("storage"),
    )
    .with_timeouts(Duration::from_secs(10), Duration::from_secs(600));
    let connection = StdioSidecarConnection::new(config);
    let payload = serde_json::json!({
        "sidecar_pid_file": state_dir.join("sidecar.pid"),
        "child_pid_file": state_dir.join("child.pid"),
    });
    connection.invoke("hang", &payload.to_string())?;
    bail!("hang 请求意外结束")
}
