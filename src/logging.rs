use anyhow::Result;
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// 日志文件前缀
const LOG_FILE_PREFIX: &str = "tiangong";

/// 默认日志过滤规则
const DEFAULT_LOG_FILTER: &str = "tiangong=error";

/// 初始化日志系统
///
/// 支持 `TIANGONG_LOG` 环境变量自定义日志级别，例如：
/// - `TIANGONG_LOG=debug` — 全局 debug
/// - `TIANGONG_LOG=tiangong_core=debug,tiangong_server=info` — 按模块分级
/// - 未设置时默认仅记录 ERROR 到文件
///
/// 返回 WorkerGuard，需要在 main 函数中保持存活以确保日志刷新
pub fn init_logging() -> Result<WorkerGuard> {
    let log_dir = get_log_dir()?;
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, LOG_FILE_PREFIX);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // 优先使用 TIANGONG_LOG 环境变量，回退到默认
    let filter = EnvFilter::try_from_env("TIANGONG_LOG")
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false)
                .with_file(true)
                .with_line_number(true),
        )
        .init();

    Ok(guard)
}

/// 获取日志目录路径
fn get_log_dir() -> Result<PathBuf> {
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
