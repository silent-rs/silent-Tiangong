use anyhow::Result;
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// 日志文件前缀
const LOG_FILE_PREFIX: &str = "tiangong-error";

/// 初始化日志系统
///
/// 将 ERROR 级别的日志输出到 ~/.tiangong/logs 目录，按天分割文件
/// 返回 WorkerGuard，需要在 main 函数中保持存活以确保日志刷新
pub fn init_logging() -> Result<WorkerGuard> {
    // 获取日志目录
    let log_dir = get_log_dir()?;

    // 确保日志目录存在
    std::fs::create_dir_all(&log_dir)?;

    // 使用 tracing-appender 的非阻塞写入
    // 按天轮转：每天创建新文件
    let file_appender = tracing_appender::rolling::daily(&log_dir, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // 配置订阅器
    // 仅记录 ERROR 级别的日志到文件
    let filter = EnvFilter::new("tiangong=error");

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false) // 文件输出不需要 ANSI 颜色
                .with_target(true) // 包含目标模块
                .with_thread_ids(false) // 不包含线程 ID
                .with_file(true) // 包含文件名
                .with_line_number(true), // 包含行号
        )
        .init();

    Ok(guard)
}

/// 获取日志目录路径
fn get_log_dir() -> Result<PathBuf> {
    // 使用标准库获取主目录
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("无法获取用户主目录"))?;

    Ok(PathBuf::from(home).join(".tiangong").join("logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_log_dir() {
        let log_dir = get_log_dir().unwrap();
        assert!(log_dir.ends_with(".tiangong/logs"));
    }
}
