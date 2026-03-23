use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow};

/// 获取用户 home 目录
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| v != OsStr::new("")) {
        return Some(PathBuf::from(profile));
    }
    None
}

/// PID 文件路径: ~/.tiangong/server.pid
fn pid_file_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.pid")
}

/// 后台运行 Server：重新启动自身进程并加上 --foreground 标志，主进程退出
pub fn run_daemon(host: &str, port: u16, token: Option<String>) -> Result<()> {
    let exe = std::env::current_exe().context("获取当前可执行文件路径失败")?;

    let mut cmd = Command::new(&exe);
    cmd.arg("server")
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string());

    if let Some(ref t) = token {
        cmd.arg("--token").arg(t);
    }

    // 不加 --daemon，这样子进程会作为前台进程运行
    // 但主进程会退出，子进程在后台继续

    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("启动后台 Server 进程失败")?;

    let pid = child.id();

    // 写入 PID 文件
    let pid_path = pid_file_path();
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&pid_path, pid.to_string()).context("写入 PID 文件失败")?;

    tracing::info!("Server 已在后台启动，PID: {pid}");
    println!("Server 已在后台启动，PID: {pid}，监听 {host}:{port}");

    Ok(())
}

/// 停止后台 Server 进程
pub fn stop_daemon() -> Result<()> {
    let pid_path = pid_file_path();
    if !pid_path.exists() {
        return Err(anyhow!("PID 文件不存在，Server 可能未在后台运行"));
    }

    let pid_str = fs::read_to_string(&pid_path).context("读取 PID 文件失败")?;
    let pid: u32 = pid_str.trim().parse().context("PID 文件内容无效")?;

    // 发送 SIGTERM
    #[cfg(unix)]
    {
        use std::process::Command;
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .context("发送 SIGTERM 失败")?;

        if status.success() {
            println!("已向 PID {pid} 发送 SIGTERM");
        } else {
            println!("发送信号失败，进程可能已退出");
        }
    }

    #[cfg(not(unix))]
    {
        return Err(anyhow!(
            "非 Unix 平台暂不支持 stop 命令，请手动终止 PID {pid}"
        ));
    }

    // 清理 PID 文件
    fs::remove_file(&pid_path).ok();

    Ok(())
}
