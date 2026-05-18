use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

fn daemon_log_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("logs")
        .join("server-daemon.log")
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

    #[cfg(unix)]
    {
        // 让后台 Server 脱离父进程会话，避免 GUI 进程状态变化影响子进程。
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    // 不加 --daemon，这样子进程会作为前台进程运行
    // 但主进程会退出，子进程在后台继续

    let log_path = daemon_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("打开 Server 后台日志失败")?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("打开 Server 后台日志失败")?;

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("启动后台 Server 进程失败")?;

    let pid = child.id();
    if let Some(status) = child.try_wait().context("检查后台 Server 进程状态失败")? {
        return Err(anyhow!("后台 Server 启动后立即退出：{status}"));
    }

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
            fs::remove_file(&pid_path).ok();
            return Err(anyhow!("发送 SIGTERM 失败，进程可能已退出"));
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
