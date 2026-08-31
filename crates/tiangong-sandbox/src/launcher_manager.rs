//! App 集成侧的 Sandbox 程序定位。
//!
//! 生产布局固定为 `<storage_root>/sandbox/tiangong-sandbox[.exe]` 及同目录
//! 伴生签名。版本由程序自身 `--self-check` 报告，不再通过版本目录或
//! active/pending 指针表达。调试构建仍优先使用宿主同目录的本地构建。

use std::path::{Path, PathBuf};

/// storage_root 下的 Sandbox 安装目录。
pub const LAUNCHER_DIR: &str = "sandbox";

pub fn builtin_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn launcher_file_name() -> String {
    format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX)
}

pub fn install_directory(storage_root: &Path) -> PathBuf {
    storage_root.join(LAUNCHER_DIR)
}

pub fn installed_sandbox(storage_root: &Path) -> PathBuf {
    install_directory(storage_root).join(launcher_file_name())
}

pub fn signature_path(program: &Path) -> PathBuf {
    program.with_file_name(format!(
        "{}.sig",
        program
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

/// 返回值仍需由调用方逐次验签与自检。
pub fn resolve_sandbox_binary(storage_root: &Path) -> Option<PathBuf> {
    select_sandbox_binary(regular_installed_sandbox(storage_root), builtin_sandbox())
}

fn select_sandbox_binary(installed: Option<PathBuf>, local: Option<PathBuf>) -> Option<PathBuf> {
    installed.or(local)
}

fn regular_installed_sandbox(storage_root: &Path) -> Option<PathBuf> {
    if !storage_root.is_absolute() {
        return None;
    }
    let program = installed_sandbox(storage_root);
    let program_meta = std::fs::symlink_metadata(&program).ok()?;
    if !program_meta.is_file() || program_meta.file_type().is_symlink() {
        return None;
    }
    let signature_meta = std::fs::symlink_metadata(signature_path(&program)).ok()?;
    if !signature_meta.is_file() || signature_meta.file_type().is_symlink() {
        return None;
    }
    Some(program)
}

fn builtin_sandbox() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let directory = current_exe.parent()?;
    let direct = directory.join(launcher_file_name());
    if direct.is_file() {
        return Some(direct);
    }
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
    fn installed_layout_is_flat() {
        let root = Path::new("/storage");
        assert_eq!(
            installed_sandbox(root),
            root.join("sandbox").join(launcher_file_name())
        );
        assert!(
            !installed_sandbox(root)
                .to_string_lossy()
                .contains("versions")
        );
    }

    #[test]
    fn resolver_requires_program_and_signature() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install_directory(root.path())).unwrap();
        let program = installed_sandbox(root.path());
        std::fs::write(&program, b"sandbox").unwrap();
        assert!(regular_installed_sandbox(root.path()).is_none());
        std::fs::write(signature_path(&program), b"signature").unwrap();
        assert_eq!(regular_installed_sandbox(root.path()), Some(program));
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_symlink_program() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install_directory(root.path())).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"sandbox").unwrap();
        let program = installed_sandbox(root.path());
        std::os::unix::fs::symlink(target, &program).unwrap();
        std::fs::write(signature_path(&program), b"signature").unwrap();
        assert!(regular_installed_sandbox(root.path()).is_none());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_build_prefers_installed_program() {
        let local = PathBuf::from("/debug/tiangong-sandbox");
        let installed = PathBuf::from("/storage/sandbox/tiangong-sandbox");
        assert_eq!(
            select_sandbox_binary(Some(installed.clone()), Some(local)),
            Some(installed)
        );
    }
}
