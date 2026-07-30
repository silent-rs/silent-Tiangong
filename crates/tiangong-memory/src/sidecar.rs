//! Memory sidecar 进程管理器。
//!
//! 负责 Core 启动时自动拉起 memory sidecar 进程（如果二进制存在），
//! 并等待其就绪（endpoint 文件出现）。sidecar 不存在时优雅降级为
//! 进程内 actor（现有 Leader 模式兜底）。
//!
//! 见 RFC docs/memory-system/11-memory-sidecar-wasm-bridge.md。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::injection::memory_base_dir;
use crate::ipc;

/// sidecar 二进制在 storage_root 下的固定路径。
const SIDECAR_DIR: &str = "memory-sidecar";
const SIDECAR_BIN: &str = "tiangong-memory-sidecar";

/// Memory sidecar 进程管理器。
pub struct MemorySidecarManager {
    storage_root: PathBuf,
}

impl MemorySidecarManager {
    pub fn new() -> Self {
        Self {
            storage_root: memory_base_dir(),
        }
    }

    /// sidecar 二进制路径（~/.tiangong/memory-sidecar/tiangong-memory-sidecar）。
    fn sidecar_binary(&self) -> PathBuf {
        let mut path = self.storage_root.clone();
        path.pop(); // 从 memory/ 回到 .tiangong/
        path.push(SIDECAR_DIR);
        path.push(SIDECAR_BIN);
        path
    }

    /// sidecar 二进制是否存在（可用于决定是否启用 sidecar 模式）。
    pub fn binary_exists(&self) -> bool {
        self.sidecar_binary().exists()
    }

    /// 确保 sidecar 进程在运行：没在跑就 spawn，等就绪。
    ///
    /// 返回 Ok 表示 sidecar 已就绪（endpoint 文件可读）；
    /// 返回 Err 表示 sidecar 不可用（二进制缺失/spawn 失败/超时），
    /// 调用方应降级为进程内 actor。
    pub async fn ensure_running(&self) -> Result<()> {
        let service = crate::election::memory_service_name();

        // 已在运行则直接返回。
        if self.is_running(&service) {
            return Ok(());
        }

        let binary = self.sidecar_binary();
        if !binary.exists() {
            anyhow::bail!("sidecar 二进制不存在: {}", binary.display());
        }

        tracing::info!("启动 memory sidecar: {}", binary.display());
        self.spawn(&binary).await?;
        self.wait_ready(&service, Duration::from_secs(15)).await?;
        tracing::info!("memory sidecar 已就绪");
        Ok(())
    }

    /// 检查 sidecar 是否在运行（endpoint 文件可读）。
    fn is_running(&self, service: &str) -> bool {
        ipc::load_endpoint(service).is_ok()
    }

    /// spawn sidecar 二进制（detach，sidecar 自己维持运行）。
    async fn spawn(&self, binary: &PathBuf) -> Result<()> {
        use tokio::process::Command;

        let mut cmd = Command::new(binary);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // Windows 防窗口闪现。
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("spawn sidecar 失败: {}", binary.display()))?;
        tracing::info!("memory sidecar 已启动（pid={}）", child.id().unwrap_or(0));
        // detach：不等待子进程，sidecar 自行运行。
        std::mem::forget(child);
        Ok(())
    }

    /// 等待 sidecar 就绪（endpoint 文件出现），超时则报错。
    async fn wait_ready(&self, service: &str, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.is_running(service) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("等待 sidecar 就绪超时（{}s）", timeout.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Default for MemorySidecarManager {
    fn default() -> Self {
        Self::new()
    }
}
