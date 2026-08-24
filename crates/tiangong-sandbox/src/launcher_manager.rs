//! 随 App 发布的固定 Launcher 路径解析。
//!
//! 当前生产链只接受宿主可执行文件同目录的 `tiangong-sandbox`。独立下载、
//! active 版本切换与回滚在可信更新链完成前不进入执行路径。

use std::path::{Path, PathBuf};

/// 解析随宿主安装的 Launcher。`storage_root` 保留在签名中，避免调用方在
/// 后续独立更新方案落地前反复改接口；当前不会从可写数据目录加载程序。
pub fn resolve_sandbox_binary(_storage_root: &Path) -> Option<PathBuf> {
    builtin_sandbox()
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

fn launcher_file_name() -> String {
    format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_never_reads_writable_runtime_directory() {
        let root = tempfile::tempdir().unwrap();
        let writable = root.path().join("runtime/sandbox/tiangong-sandbox");
        std::fs::create_dir_all(writable.parent().unwrap()).unwrap();
        std::fs::write(&writable, b"stub").unwrap();

        assert_ne!(resolve_sandbox_binary(root.path()), Some(writable));
    }
}
