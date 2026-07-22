//! bot 日志——合并 stdout/stderr 写入文件，带轮转与内存错误摘要。
//!
//! - stdout、stderr 合并写入 `~/.tiangong/bots/<id>/bot.log`，每行标注来源与时间。
//! - 崩溃重启后追加（不覆盖）。
//! - 单文件达到大小上限时轮转，保留最近 N 个。
//! - 内存只保留最近 [`ERROR_SUMMARY_BYTES`] 的 stderr 摘要供健康状态展示。
//! - 写日志失败不导致 bot 退出，仅在主程序 `tracing::warn` 中报告。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

/// 内存错误摘要缓冲上限（字节）。
const ERROR_SUMMARY_BYTES: usize = 8 * 1024;

/// 单个日志文件大小上限（字节），超出后触发轮转。
const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// 保留的轮转文件数量（不含当前 bot.log）。
const LOG_KEEP_ROTATED: usize = 3;

/// 日志来源标签。
#[derive(Clone, Copy)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

impl StreamKind {
    fn label(self) -> &'static str {
        match self {
            StreamKind::Stdout => "stdout",
            StreamKind::Stderr => "stderr",
        }
    }
}

/// bot 日志写入器——线程安全，可在多个异步 task 间共享。
pub struct BotLogger {
    /// 日志文件路径。
    path: PathBuf,
    /// 内存错误摘要（仅 stderr，最近 ERROR_SUMMARY_BYTES）。
    error_summary: Arc<Mutex<String>>,
    /// 当前写入的字节数（用于判断轮转）。
    written: Arc<Mutex<u64>>,
}

impl BotLogger {
    /// 创建日志写入器。日志目录会在首次写入时创建。
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            error_summary: Arc::new(Mutex::new(String::new())),
            written: Arc::new(Mutex::new(0)),
        }
    }

    /// 取最近的错误摘要（供健康状态展示）。
    pub async fn error_summary(&self) -> String {
        self.error_summary.lock().await.clone()
    }

    /// 写入一行日志（标注来源与时间）。
    ///
    /// 写入失败仅在主程序日志告警，不返回错误——日志不能影响 bot 生命周期。
    pub async fn write_line(&self, kind: StreamKind, raw: &[u8]) {
        let timestamp = chrono::Local::now()
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S%.3f");
        // 按行写入（bot 输出可能不带换行）。
        let line = String::from_utf8_lossy(raw);
        let entry = format!("[{}] {} {}\n", timestamp, kind.label(), line.trim_end());

        // 写入文件（失败仅告警）。
        if let Err(err) = self.write_to_file(&entry).await {
            tracing::warn!("写入 bot 日志失败（{}）：{err}", self.path.display());
        }

        // stderr 更新内存摘要。
        if matches!(kind, StreamKind::Stderr) {
            self.update_summary(&entry).await;
        }
    }

    /// 持续消费一个流（stdout 或 stderr），写入日志文件。
    ///
    /// 返回 JoinHandle，流读完后结束。
    pub fn consume_stream(
        self: Arc<Self>,
        stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
        kind: StreamKind,
    ) -> tokio::task::JoinHandle<()> {
        let Some(stream) = stream else {
            return tokio::spawn(async {});
        };
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut reader = stream;
            let mut buf = vec![0u8; 4096];
            let mut line_buf: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        // flush 残留的最后一行。
                        if !line_buf.is_empty() {
                            self.write_line(kind, &line_buf).await;
                        }
                        break;
                    }
                    Ok(n) => {
                        line_buf.extend_from_slice(&buf[..n]);
                        // 按换行切分，每行写一条日志。
                        while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = line_buf.drain(..=pos).collect();
                            self.write_line(kind, &line).await;
                        }
                    }
                }
            }
        })
    }

    /// 写入文件并在超限时轮转。
    async fn write_to_file(&self, entry: &str) -> std::io::Result<()> {
        let mut guard = self.written.lock().await;
        let bytes = entry.len() as u64;

        // 写入前检查是否需要轮转。
        if *guard + bytes > LOG_MAX_BYTES {
            drop(guard);
            self.rotate();
            *self.written.lock().await = 0;
            guard = self.written.lock().await;
        }

        // 追加写入（O_APPEND | O_CREAT）。
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(entry.as_bytes())?;
        *guard += bytes;
        Ok(())
    }

    /// 更新内存错误摘要（只保留最后 ERROR_SUMMARY_BYTES）。
    async fn update_summary(&self, entry: &str) {
        let mut guard = self.error_summary.lock().await;
        guard.push_str(entry);
        if guard.len() > ERROR_SUMMARY_BYTES {
            let cut = guard.len() - ERROR_SUMMARY_BYTES;
            *guard = guard[cut..].to_string();
        }
    }

    /// 轮转：bot.log → bot.log.1 → bot.log.2 → ... 删除最旧。
    fn rotate(&self) {
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        // 从旧到新依次重命名：.2 → .3（删除），.1 → .2，.log → .1。
        for i in (1..=LOG_KEEP_ROTATED).rev() {
            let src = if i == LOG_KEEP_ROTATED {
                // 最旧文件直接删除。
                let oldest = dir.join(format!("bot.log.{}", i));
                let _ = std::fs::remove_file(&oldest);
                continue;
            } else {
                dir.join(format!("bot.log.{i}"))
            };
            let dst = dir.join(format!("bot.log.{}", i + 1));
            let _ = std::fs::rename(&src, &dst);
        }
        // bot.log → bot.log.1。
        let dst = dir.join("bot.log.1");
        let _ = std::fs::rename(&self.path, &dst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn write_and_read_summary() {
        let dir = TempDir::new().unwrap();
        let logger = BotLogger::new(dir.path().join("bot.log"));
        logger
            .write_line(StreamKind::Stderr, b"connection error\n")
            .await;
        logger.write_line(StreamKind::Stdout, b"started\n").await;

        let summary = logger.error_summary().await;
        assert!(summary.contains("connection error"));
        // stdout 不进摘要。
        assert!(!summary.contains("started"));

        // 文件包含两条。
        let content = std::fs::read_to_string(dir.path().join("bot.log")).unwrap();
        assert!(content.contains("stderr") && content.contains("connection error"));
        assert!(content.contains("stdout") && content.contains("started"));
    }

    #[tokio::test]
    async fn append_not_overwrite() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bot.log");
        std::fs::write(&path, "pre-existing\n").unwrap();

        let logger = BotLogger::new(path);
        logger.write_line(StreamKind::Stderr, b"new line\n").await;

        let content = std::fs::read_to_string(dir.path().join("bot.log")).unwrap();
        assert!(content.contains("pre-existing"));
        assert!(content.contains("new line"));
    }

    #[tokio::test]
    async fn summary_truncates_to_limit() {
        let dir = TempDir::new().unwrap();
        let logger = BotLogger::new(dir.path().join("bot.log"));
        // 写入超过 ERROR_SUMMARY_BYTES 的 stderr。
        let big = "x".repeat(ERROR_SUMMARY_BYTES + 1000);
        logger.write_line(StreamKind::Stderr, big.as_bytes()).await;
        let summary = logger.error_summary().await;
        assert!(summary.len() <= ERROR_SUMMARY_BYTES + 100); // 容忍一行多出的前缀
    }

    #[tokio::test]
    async fn consume_stream_handles_no_trailing_newline() {
        let dir = TempDir::new().unwrap();
        let logger = Arc::new(BotLogger::new(dir.path().join("bot.log")));
        // 模拟一个无换行结尾的流。
        let data = b"line without newline".to_vec();
        let cursor = std::io::Cursor::new(data);
        let handle = logger
            .clone()
            .consume_stream(Some(cursor), StreamKind::Stdout);
        handle.await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("bot.log")).unwrap();
        assert!(content.contains("line without newline"));
    }
}
