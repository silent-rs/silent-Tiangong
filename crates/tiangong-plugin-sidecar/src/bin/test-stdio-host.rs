//! 测试用 stdio 宿主：启动 sidecar 并保持一个长请求运行。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tiangong_plugin_runtime::sidecar::{EphemeralCommandConnection, SidecarInvocationContext};
use tiangong_plugin_runtime::sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().context("缺少运行模式或 sidecar 路径")?;
    if first == "--command-sandbox" {
        let sidecar = args
            .next()
            .map(PathBuf::from)
            .context("缺少 command sidecar 路径")?;
        let state_dir = args.next().map(PathBuf::from).context("缺少状态目录")?;
        if args.next().is_some() {
            bail!("参数过多");
        }
        return run_command_sandbox(sidecar, state_dir);
    }
    let sidecar = PathBuf::from(first);
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

fn run_command_sandbox(sidecar: PathBuf, state_dir: PathBuf) -> Result<()> {
    let storage_root = state_dir.join("storage");
    let plugin_root = storage_root.join("plugins/command");
    let workspace = state_dir.join("workspace");
    let home = state_dir.join("home");
    std::fs::create_dir_all(&plugin_root).context("创建 command 插件目录失败")?;
    std::fs::create_dir_all(&workspace).context("创建 command 工作区失败")?;
    std::fs::create_dir_all(&home).context("创建测试 HOME 失败")?;
    let binary_name = format!("tiangong-command-sidecar{}", std::env::consts::EXE_SUFFIX);
    let installed_sidecar = plugin_root.join(&binary_name);
    std::fs::copy(&sidecar, &installed_sidecar).context("复制 command sidecar 失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed_sidecar, std::fs::Permissions::from_mode(0o755))
            .context("设置 command sidecar 权限失败")?;
    }
    std::fs::write(
        plugin_root.join("plugin.json"),
        serde_json::json!({
            "id": "command",
            "sidecar": { "binary": "tiangong-command-sidecar" }
        })
        .to_string(),
    )
    .context("写入 command 插件清单失败")?;

    let config = SidecarConfig::new(
        "command",
        "0.0.0",
        installed_sidecar,
        plugin_root.join("endpoint.json"),
        plugin_root.join("sidecar.log"),
        plugin_root.join("data"),
        storage_root,
    )
    .with_timeouts(Duration::from_secs(15), Duration::from_secs(600))
    .with_sandbox_program_root(Some(plugin_root));
    let connection = EphemeralCommandConnection::new(config);
    connection.update_exec_env(BTreeMap::from([(
        "HOME".to_string(),
        home.display().to_string(),
    )]));
    let sidecar_pid_file = workspace.join("sidecar.pid");
    let child_pid_file = workspace.join("child.pid");
    let script = format!(
        "/usr/bin/printf '%s' \"$PPID\" > {}; /bin/sleep 300 & child=$!; /usr/bin/printf '%s' \"$child\" > {}; wait \"$child\"",
        shell_quote(&sidecar_pid_file),
        shell_quote(&child_pid_file),
    );
    let request = serde_json::json!({
        "script": script,
        "shell": "sh",
        "timeout_secs": 300,
        "workspace": workspace.display().to_string(),
        "full_trust": true,
        "allowed_commands": [],
    });
    let context = SidecarInvocationContext {
        session_id: "host-crash".to_string(),
        invocation_id: "host-crash".to_string(),
        authoritative_workspace: workspace,
    };
    connection.invoke_with_context("command.run_shell", &request.to_string(), &context)?;
    bail!("command 长请求意外结束")
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
