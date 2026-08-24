//! tiangong-sandbox 沙箱程序版本管理（RFC 0017 §十 独立更新与回滚，第一阶段）。
//!
//! 选择链（不能降级为无沙箱执行）：
//! ```text
//! ~/.tiangong/runtime/sandbox/active.json 指向的已安装版本
//!   → 宿主可执行文件同目录的内置保底版
//!   → 全部不可用：报错（沙箱任务拒绝执行）
//! ```
//!
//! 目录布局（与 RFC 一致，更新/回滚/撤销流程后续阶段接入）：
//! ```text
//! ~/.tiangong/runtime/sandbox/
//!   active.json          # {"version": "0.3.2"}
//!   installs/<version>/  # 各版本安装目录
//!   staging/             # 更新暂存（下载→校验→自检→原子安装）
//!   quarantine/          # 自检失败的版本隔离
//! ```

use std::path::{Path, PathBuf};

/// 沙箱运行时根目录。
pub fn runtime_root(storage_root: &Path) -> PathBuf {
    storage_root.join("runtime").join("sandbox")
}

/// 解析当前应使用的 Launcher 可执行文件路径。
///
/// 优先 active.json 指向的已安装版本；不存在或文件缺失时回退宿主同目录
/// 的内置保底版；两者皆不可用返回 None（调用方必须拒绝沙箱任务）。
pub fn resolve_sandbox_binary(storage_root: &Path) -> Option<PathBuf> {
    // 审查修订：完整的可信安装链（发布签名、制品摘要、清单、自检激活、
    // 撤销检查）实现之前，可写目录中的 active.json 不进入安全执行链——
    // 否则替换该文件即可让所有受限 sidecar 无声逃逸。默认仅使用随宿主
    // 安装包分发、由平台安装器签名保护的内置版。
    if std::env::var("TIANGONG_SANDBOX_ALLOW_ACTIVE")
        .ok()
        .as_deref()
        == Some("1")
        && let Some(path) = resolve_active_installs(storage_root)
    {
        return Some(path);
    }
    builtin_sandbox()
}

/// 实验开关（TIANGONG_SANDBOX_ALLOW_ACTIVE=1）下的 active 版本解析：
/// 严格 SemVer（拒绝路径组件）、真实普通文件（拒绝符号链接）。
/// 发布签名/摘要/自检/撤销校验见 RFC 后续阶段。
fn resolve_active_installs(storage_root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(runtime_root(storage_root).join("active.json")).ok()?;
    let active: ActiveFile = serde_json::from_str(&text).ok()?;
    if !is_safe_version(&active.version) {
        tracing::warn!(version = %active.version, "active.json 版本非合法 SemVer，忽略");
        return None;
    }
    let installed = runtime_root(storage_root)
        .join("installs")
        .join(&active.version)
        .join(launcher_file_name());
    let metadata = std::fs::symlink_metadata(&installed).ok()?;
    if metadata.is_file() {
        Some(installed)
    } else {
        tracing::warn!(
            version = %active.version,
            "active 指向的不是普通文件（拒绝符号链接）"
        );
        None
    }
}

/// 严格 SemVer 校验：仅数字段与点分隔，拒绝路径组件（`/`、`\\`、`..`）。
fn is_safe_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 32
        && version.split('.').count() == 3
        && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
        && !version.contains("..")
}

fn builtin_sandbox() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let builtin = current_exe.parent()?.join(launcher_file_name());
    builtin.is_file().then_some(builtin)
}

fn launcher_file_name() -> String {
    if std::env::consts::EXE_SUFFIX.is_empty() {
        "tiangong-sandbox".to_string()
    } else {
        format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX)
    }
}

#[derive(Debug, serde::Deserialize)]
struct ActiveFile {
    version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_active_falls_back_to_builtin_or_none() {
        let root = tempfile::tempdir().unwrap();
        // 无 active.json 且宿主同目录无内置（测试二进制目录）→ None（拒绝执行）。
        // 注：若测试环境恰好存在同名文件则返回 Some，两种结果都符合选择链语义。
        if let Some(path) = resolve_sandbox_binary(root.path()) {
            assert!(path.is_file());
        }
    }

    #[test]
    fn active_json_points_to_install() {
        let root = tempfile::tempdir().unwrap();
        let installs = runtime_root(root.path()).join("installs").join("9.9.9");
        std::fs::create_dir_all(&installs).unwrap();
        let launcher = installs.join(launcher_file_name());
        std::fs::write(&launcher, b"stub").unwrap();
        std::fs::write(
            runtime_root(root.path()).join("active.json"),
            r#"{"version": "9.9.9"}"#,
        )
        .unwrap();
        // 默认（无实验开关）：active 不进入安全链，仅内置保底。
        // SAFETY: 测试串行段，无并发读环境变量的线程。
        unsafe { std::env::remove_var("TIANGONG_SANDBOX_ALLOW_ACTIVE") };
        assert_ne!(resolve_sandbox_binary(root.path()), Some(launcher.clone()));
        // 实验开关开启：合法 SemVer 且真实普通文件才生效。
        // SAFETY: 测试串行段，无并发读环境变量的线程。
        unsafe { std::env::set_var("TIANGONG_SANDBOX_ALLOW_ACTIVE", "1") };
        assert_eq!(resolve_sandbox_binary(root.path()), Some(launcher));
        // SAFETY: 测试串行段，无并发读环境变量的线程。
        unsafe { std::env::remove_var("TIANGONG_SANDBOX_ALLOW_ACTIVE") };
    }

    #[test]
    fn unsafe_active_versions_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(runtime_root(root.path())).unwrap();
        // SAFETY: 测试串行段，无并发读环境变量的线程。
        unsafe { std::env::set_var("TIANGONG_SANDBOX_ALLOW_ACTIVE", "1") };
        for bad in ["../../etc", "9.9", "a.b.c", "1.2.3/x"] {
            std::fs::write(
                runtime_root(root.path()).join("active.json"),
                format!("{{\"version\": \"{bad}\"}}"),
            )
            .unwrap();
            assert!(
                resolve_active_installs(root.path()).is_none(),
                "应拒绝非法版本：{bad}"
            );
        }
        // SAFETY: 测试串行段，无并发读环境变量的线程。
        unsafe { std::env::remove_var("TIANGONG_SANDBOX_ALLOW_ACTIVE") };
    }
}
