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

/// Memory sidecar 连接实现（通用 SidecarConnection trait）。
///
/// 包装 MemoryHandle，将 WASM 插件的 sidecar.invoke 转发到
/// MemoryHandle::ipc_request。入口侧构造后注入给 plugin-runtime。
pub struct MemorySidecarConnection {
    handle: crate::MemoryHandle,
    runtime: tokio::runtime::Runtime,
}

impl MemorySidecarConnection {
    pub fn new(handle: crate::MemoryHandle) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 sidecar 连接 runtime 失败");
        Self { handle, runtime }
    }
}

impl tiangong_plugin_runtime::SidecarConnection for MemorySidecarConnection {
    fn invoke(&self, payload: &str) -> anyhow::Result<String> {
        use crate::ipc::protocol::{MemoryIpcRequestPayload, MemoryIpcResponsePayload};

        // WASM 组件发送的信封格式：{"method": "...", "payload": {...}}
        // 需要解包：把 payload 的内容合并到顶层，让它匹配 MemoryIpcRequestPayload
        //（后者用 #[serde(tag = "method")]，期望 method 和业务字段在同一个 JSON 对象里）。
        let envelope: serde_json::Value = serde_json::from_str(payload)
            .map_err(|e| anyhow::anyhow!("解析 sidecar 信封失败: {e}"))?;

        // 构造 MemoryIpcRequestPayload 兼容的 JSON：method 从信封取，其余从 payload 取。
        let method = envelope
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let inner = envelope
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // 合并：以 inner 为基础，把 method 写进去（确保 tag 正确）。
        let mut merged = if inner.is_object() {
            inner
        } else {
            serde_json::json!({})
        };
        if let Some(obj) = merged.as_object_mut() {
            obj.insert(
                "method".to_string(),
                serde_json::Value::String(method.to_string()),
            );
        }

        let request: MemoryIpcRequestPayload = serde_json::from_value(merged)
            .map_err(|e| anyhow::anyhow!("解析 MemoryIpcRequestPayload 失败: {e}"))?;

        let handle = self.handle.clone();
        let response: MemoryIpcResponsePayload = self
            .runtime
            .handle()
            .block_on(async move { handle.ipc_request(request).await })?;
        serde_json::to_string(&response)
            .map_err(|e| anyhow::anyhow!("序列化 sidecar 响应失败: {e}"))
    }
}
