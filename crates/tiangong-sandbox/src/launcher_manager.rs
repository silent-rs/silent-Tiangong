//! 沙箱 Launcher 的解析与在线更新版本管理。
//!
//! 布局（storage_root 下）：`sandbox/versions/<版本>/` 存放下载版制品与
//! 伴生签名，`sandbox/active` 是激活版本指针（内容为版本号）。
//!
//! 解析优先级：
//! 1. active 指向的下载版（每次启动前仍由宿主逐次验签）——仅当版本
//!    单调（≥ 内置基准版本）且制品与签名齐备时生效；active 是用户可写
//!    文件，指向旧版或制品缺失时解析落空，防降级与指针篡改。
//! 2. 宿主可执行文件同目录的同名程序（可选来源：开发环境构建产物，
//!    或集成方选择随包分发时生效）；不随包分发时无此来源，Launcher
//!    由在线更新链获取，未就绪前宿主按 fail-closed 拒绝沙箱执行。
//!
//! 版本单调的基准是编译期 crate 版本（builtin_version）：它是宿主
//! 进程内策略编译器所配套的最低 Launcher 版本，与磁盘上是否存在
//! 内置制品无关。
//!
//! Launcher 使用独立版本序列（见本 crate Cargo.toml），不随 App 版本；
//! “下载版 ≥ 内置版”在各自独立演进下语义自洽：App 升级携带更新内置版
//! 后，旧下载版自动让位。

use std::path::{Path, PathBuf};

/// storage_root 下的 Launcher 数据目录名（与更新器共用）。
pub const LAUNCHER_DIR: &str = "sandbox";
/// 激活版本指针文件名（内容为版本号文本）。
pub const LAUNCHER_ACTIVE_FILE: &str = "active";

/// 内置 Launcher 版本（本 crate 独立版本序列，与 App 版本无关）。
/// 在线更新器以此为防降级底线与更新判定基准。
pub fn builtin_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 解析当前应使用的 Launcher。优先在线更新激活的下载版（版本单调且
/// 制品齐备），否则内置版。
pub fn resolve_sandbox_binary(storage_root: &Path) -> Option<PathBuf> {
    if let Some(downloaded) = resolve_downloaded(storage_root) {
        return Some(downloaded);
    }
    builtin_sandbox()
}

/// 解析在线更新激活的下载版；任何不满足（无指针、版本旧、制品缺、
/// 路径非法）都返回 None 回退内置版。
fn resolve_downloaded(storage_root: &Path) -> Option<PathBuf> {
    if !storage_root.is_absolute() {
        return None;
    }
    let active =
        std::fs::read_to_string(storage_root.join(LAUNCHER_DIR).join(LAUNCHER_ACTIVE_FILE)).ok()?;
    let version = active.trim();
    if !is_safe_version(version) {
        return None;
    }
    // 版本单调：下载版不得旧于内置版（active 为用户可写文件，指针可能
    // 被改向带已知漏洞的旧官方制品）。
    if !version_at_least_builtin(version) {
        return None;
    }
    let binary = storage_root
        .join(LAUNCHER_DIR)
        .join("versions")
        .join(version)
        .join(launcher_file_name());
    if !binary.is_file() {
        return None;
    }
    // 伴生签名必须同在（宿主启动前逐次验签会再校验内容，这里只挡缺失）。
    if !signature_path(&binary).is_file() {
        return None;
    }
    // 符号链接防替换：下载版必须是普通文件。
    if std::fs::symlink_metadata(&binary).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return None;
    }
    Some(binary)
}

/// 下载版制品的伴生签名路径（与验签侧 `<launcher>.sig` 约定一致）。
pub fn signature_path(launcher: &Path) -> PathBuf {
    launcher.with_file_name(format!(
        "{}.sig",
        launcher
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

fn launcher_file_name() -> String {
    format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX)
}

/// 版本字符串只允许 x.y.z 数值形态，防止路径注入。
fn is_safe_version(version: &str) -> bool {
    parse_version(version).is_some()
}

/// 下载版本 ≥ 内置版本（Launcher 独立序列）。解析失败一律拒绝
/// （fail-closed 回退内置版）。
fn version_at_least_builtin(version: &str) -> bool {
    match (
        parse_version(version),
        parse_version(env!("CARGO_PKG_VERSION")),
    ) {
        (Some(candidate), Some(builtin)) => candidate >= builtin,
        _ => false,
    }
}

/// 极简 semver 数值比较（x.y.z 三段）。
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn builtin_sandbox() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let directory = current_exe.parent()?;
    let direct = directory.join(launcher_file_name());
    if direct.is_file() {
        return Some(direct);
    }

    // `cargo test` 的集成测试位于 target/<profile>/deps，Launcher 位于其父目录。
    // 该回退只存在于调试构建，不进入发布制品选择链。
    #[cfg(debug_assertions)]
    if directory.file_name().is_some_and(|name| name == "deps") {
        let debug_binary = directory.parent()?.join(launcher_file_name());
        if debug_binary.is_file() {
            return Some(debug_binary);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_never_reads_writable_runtime_directory_when_no_active() {
        let root = tempfile::tempdir().unwrap();
        // 无 active 指针：解析结果绝不是数据目录里的文件。
        let resolved = resolve_sandbox_binary(root.path());
        assert_ne!(
            resolved,
            Some(root.path().join("runtime/sandbox/tiangong-sandbox"))
        );
    }

    #[test]
    fn active_version_resolution_matrix() {
        let root = tempfile::tempdir().unwrap();
        let launcher_dir = root.path().join(LAUNCHER_DIR);
        let versions = launcher_dir.join("versions");

        // 高于内置版本的下载版：命中。
        let dir = versions.join("999.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(launcher_file_name());
        std::fs::write(&binary, b"launcher").unwrap();
        std::fs::write(signature_path(&binary), b"sig").unwrap();
        std::fs::create_dir_all(&launcher_dir).unwrap();
        std::fs::write(launcher_dir.join(LAUNCHER_ACTIVE_FILE), "999.0.0").unwrap();
        assert_eq!(resolve_sandbox_binary(root.path()), Some(binary.clone()));

        // 指向低于内置版本的旧版（防降级）：回退内置。
        std::fs::write(launcher_dir.join(LAUNCHER_ACTIVE_FILE), "0.0.1").unwrap();
        assert_ne!(resolve_sandbox_binary(root.path()), Some(binary.clone()));

        // 制品缺失：回退内置。
        std::fs::write(launcher_dir.join(LAUNCHER_ACTIVE_FILE), "999.0.0").unwrap();
        std::fs::remove_file(&binary).unwrap();
        assert_ne!(resolve_sandbox_binary(root.path()), Some(binary.clone()));

        // 版本串带路径注入字符：回退内置。
        std::fs::write(&binary, b"launcher").unwrap();
        std::fs::write(signature_path(&binary), b"sig").unwrap();
        std::fs::write(
            launcher_dir.join(LAUNCHER_ACTIVE_FILE),
            "999.0.0/../../evil",
        )
        .unwrap();
        assert_ne!(resolve_sandbox_binary(root.path()), Some(binary.clone()));

        // 符号链接制品：回退内置。
        std::fs::remove_file(&binary).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", &binary).unwrap();
        std::fs::write(launcher_dir.join(LAUNCHER_ACTIVE_FILE), "999.0.0").unwrap();
        assert_ne!(resolve_sandbox_binary(root.path()), Some(binary.clone()));
    }

    #[test]
    fn version_parsing_and_monotonicity() {
        assert_eq!(parse_version("0.15.2"), Some((0, 15, 2)));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("abc"), None);
        assert!(version_at_least_builtin(env!("CARGO_PKG_VERSION")));
        assert!(!version_at_least_builtin("0.0.1"));
        assert!(!version_at_least_builtin("bad"));
    }
}
