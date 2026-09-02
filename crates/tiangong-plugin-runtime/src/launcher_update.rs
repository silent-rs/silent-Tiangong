//! App 对 Sandbox 自管理安装能力的薄封装。
//!
//! App 不维护版本目录或 active/pending 指针。首次准备与手动更新都复用
//! Sandbox 的 SelfUpdater，把程序和签名直接安装到 storage_root/sandbox。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

pub const LAUNCHER_MANIFEST_ENDPOINT: &str = tiangong_sandbox::update::DEFAULT_MANIFEST_URL;
const MANIFEST_URL_OVERRIDE: &str = "TIANGONG_LAUNCHER_MANIFEST_URL";

fn launcher_update_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxLauncherStatus {
    Missing,
    Preparing,
    Ready,
    Failed,
}
impl SandboxLauncherStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
    pub fn sidecars_allowed(self) -> bool {
        self == Self::Ready
    }
}

fn startup_prepare_flags() -> &'static (
    std::sync::atomic::AtomicBool,
    std::sync::Mutex<Option<String>>,
) {
    static FLAGS: OnceLock<(
        std::sync::atomic::AtomicBool,
        std::sync::Mutex<Option<String>>,
    )> = OnceLock::new();
    FLAGS.get_or_init(|| {
        (
            std::sync::atomic::AtomicBool::new(false),
            std::sync::Mutex::new(None),
        )
    })
}
pub fn mark_launcher_preparing(value: bool) {
    startup_prepare_flags()
        .0
        .store(value, std::sync::atomic::Ordering::Release);
}
pub fn record_startup_prepare_failure(value: Option<String>) {
    if let Ok(mut slot) = startup_prepare_flags().1.lock() {
        *slot = value;
    }
}
pub fn startup_prepare_failure_reason() -> Option<String> {
    startup_prepare_flags()
        .1
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
}

pub fn launcher_status(storage_root: &Path) -> (SandboxLauncherStatus, Option<String>) {
    if startup_prepare_flags()
        .0
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return (SandboxLauncherStatus::Preparing, None);
    }
    if launcher_available(storage_root) {
        return (
            SandboxLauncherStatus::Ready,
            installed_version(storage_root),
        );
    }
    let status = if startup_prepare_failure_reason().is_some() {
        SandboxLauncherStatus::Failed
    } else {
        SandboxLauncherStatus::Missing
    };
    (status, None)
}

pub struct LauncherUpdater {
    manifest_url: String,
}
impl Default for LauncherUpdater {
    fn default() -> Self {
        Self::new()
    }
}
impl LauncherUpdater {
    pub fn new() -> Self {
        let manifest_url = std::env::var(MANIFEST_URL_OVERRIDE)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| LAUNCHER_MANIFEST_ENDPOINT.to_string());
        Self { manifest_url }
    }
    pub fn with_manifest_url(manifest_url: String) -> Self {
        Self { manifest_url }
    }
    pub async fn install_or_update(&self, storage_root: &Path) -> Result<String> {
        let _guard = launcher_update_lock().lock().await;
        let directory = host_install_directory(storage_root);
        let status = tiangong_sandbox::update::SelfUpdater::new(self.manifest_url.clone())?
            .install_to(&directory)
            .await?;
        let version = match status {
            tiangong_sandbox::update::UpdateStatus::Installed { version, .. }
            | tiangong_sandbox::update::UpdateStatus::Updated { version, .. } => version,
            tiangong_sandbox::update::UpdateStatus::UpToDate { current } => current,
            tiangong_sandbox::update::UpdateStatus::Available { version, .. } => version,
            #[cfg(windows)]
            tiangong_sandbox::update::UpdateStatus::UpdateScheduled { version, .. } => version,
        };
        cleanup_legacy_layout(storage_root);
        Ok(version)
    }
}

/// 天工宿主的 Sandbox 安装目录：直存布局 `<storage>/sandbox`。
/// P1 通用化后 crate 不再拼接存储根，目录由宿主决定。
fn host_install_directory(storage_root: &Path) -> PathBuf {
    storage_root.join("sandbox")
}
fn cleanup_legacy_layout(storage_root: &Path) {
    let directory = host_install_directory(storage_root);
    for file in ["active", "pending"] {
        let _ = std::fs::remove_file(directory.join(file));
    }
    for child in ["versions", ".transactions"] {
        let _ = std::fs::remove_dir_all(directory.join(child));
    }
}

pub fn launcher_available(storage_root: &Path) -> bool {
    let Some(launcher) = tiangong_sandbox::launcher_manager::resolve_installed_program(
        &host_install_directory(storage_root),
    ) else {
        return false;
    };
    crate::signature::verify_launcher_signature(&launcher, storage_root).is_ok()
        && verify_launcher_self_check(&launcher).is_ok()
}
pub fn installed_version(storage_root: &Path) -> Option<String> {
    let launcher = tiangong_sandbox::launcher_manager::resolve_installed_program(
        &host_install_directory(storage_root),
    )?;
    verify_launcher_self_check(&launcher).ok()
}
fn verify_launcher_self_check(binary: &Path) -> Result<String> {
    let output = std::process::Command::new(binary)
        .arg("--self-check")
        .output()
        .with_context(|| format!("运行 Sandbox 自检失败: {}", binary.display()))?;
    if !output.status.success() && output.status.code() != Some(79) {
        bail!(
            "Sandbox 自检失败（退出码 {}）：{}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("解析 Sandbox 自检报告失败")?;
    if report["protocol_version"].as_u64()
        != Some(u64::from(tiangong_sandbox::LAUNCHER_PROTOCOL_VERSION))
        || report["policy_schema"].as_u64()
            != Some(u64::from(tiangong_sandbox::LAUNCHER_POLICY_SCHEMA))
    {
        bail!("Sandbox 自报协议或策略版本与宿主不兼容");
    }
    report["product_version"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .context("Sandbox 自检报告缺少产品版本")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_removes_legacy_layout_only() {
        let root = tempfile::tempdir().unwrap();
        let sandbox = host_install_directory(root.path());
        std::fs::create_dir_all(sandbox.join("versions/0.1.0")).unwrap();
        std::fs::create_dir_all(sandbox.join(".transactions/a")).unwrap();
        std::fs::write(sandbox.join("active"), "0.1.0").unwrap();
        let program = tiangong_sandbox::launcher_manager::installed_program(
            &host_install_directory(root.path()),
        );
        std::fs::write(&program, b"program").unwrap();
        cleanup_legacy_layout(root.path());
        assert!(program.is_file());
        assert!(!sandbox.join("versions").exists());
        assert!(!sandbox.join("active").exists());
    }
}
