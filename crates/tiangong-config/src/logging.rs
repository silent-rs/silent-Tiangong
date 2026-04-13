use anyhow::Result;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub use tracing_appender::non_blocking::WorkerGuard;

pub const DEFAULT_LOG_FILE_PREFIX: &str = "tiangong";
pub const CLI_DEFAULT_LOG_FILTER: &str = "tiangong=error";
pub const DESKTOP_DEFAULT_LOG_FILTER: &str = "info";

pub struct LoggingOptions {
    pub terminal_output: bool,
    pub default_filter: &'static str,
    pub file_prefix: &'static str,
}

impl LoggingOptions {
    pub const fn cli() -> Self {
        Self {
            terminal_output: false,
            default_filter: CLI_DEFAULT_LOG_FILTER,
            file_prefix: DEFAULT_LOG_FILE_PREFIX,
        }
    }

    pub const fn desktop(terminal_output: bool) -> Self {
        Self {
            terminal_output,
            default_filter: DESKTOP_DEFAULT_LOG_FILTER,
            file_prefix: DEFAULT_LOG_FILE_PREFIX,
        }
    }
}

pub fn init_logging(options: LoggingOptions) -> Result<WorkerGuard> {
    let log_dir = get_log_dir()?;
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, options.file_prefix);
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
    let filter = EnvFilter::try_from_env("TIANGONG_LOG")
        .unwrap_or_else(|_| EnvFilter::new(options.default_filter));

    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    if options.terminal_output {
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .init();
    }

    Ok(guard)
}

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
