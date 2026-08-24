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
    // 1. active.json 指向的版本。
    if let Ok(active) = std::fs::read_to_string(runtime_root(storage_root).join("active.json"))
        && let Ok(active) = serde_json::from_str::<ActiveFile>(&active)
    {
        let installed = runtime_root(storage_root)
            .join("installs")
            .join(&active.version)
            .join(launcher_file_name());
        if installed.is_file() {
            return Some(installed);
        }
        tracing::warn!(
            version = %active.version,
            "active.json 指向的 Launcher 缺失，回退内置保底版"
        );
    }
    // 2. 宿主可执行文件同目录的内置保底版（随天工安装包分发）。
    builtin_launcher()
}

fn builtin_launcher() -> Option<PathBuf> {
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
        assert_eq!(resolve_sandbox_binary(root.path()), Some(launcher));
    }
}
