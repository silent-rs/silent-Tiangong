//! stdio 握手验证工具（RFC 0017 S2/S5 存量迁移验证）。
//!
//! 真实 spawn 指定 sidecar 二进制并完成 `runtime.handshake` 身份校验。
//!
//! 用法：
//! ```text
//! cargo run -p tiangong-plugin-runtime --example stdio_handshake -- \
//!     <sidecar 二进制路径> <plugin_id> [--sandbox]
//! ```
//!
//! `--sandbox` 按清单沙箱声明包装（宿主环境已在沙箱内时自动降级直跑并告警）。

use std::path::PathBuf;
use std::time::Duration;

use tiangong_plugin_runtime::sidecar::{SidecarConfig, StdioSidecarConnection};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("用法: stdio_handshake <sidecar 二进制路径> <plugin_id> [--sandbox]");
        std::process::exit(2);
    }
    let binary = PathBuf::from(&args[0]);
    let plugin_id = args[1].clone();
    let sandbox = args.iter().any(|arg| arg == "--sandbox");

    let base = std::env::temp_dir().join(format!(
        "stdio-handshake-{}-{plugin_id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&base).expect("创建临时目录失败");
    let config = SidecarConfig::new(
        &plugin_id,
        "0.0.0",
        binary,
        base.join("endpoint.json"),
        base.join("sidecar.log"),
        base.join("data"),
        std::env::temp_dir(),
    )
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(15))
    .with_sandbox(sandbox);

    let connection = StdioSidecarConnection::new(config);
    match connection.ensure_running_checked() {
        Ok(()) => {
            connection.stop().ok();
            println!("OK {plugin_id}: stdio 握手通过");
        }
        Err(error) => {
            connection.stop().ok();
            eprintln!("FAIL {plugin_id}: {error:#}");
            std::process::exit(1);
        }
    }
}
