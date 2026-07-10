//! Core 存储位置的类型化封装。
//!
//! 外部决定路径，core 负责路径下的存储过程（与 #208 storage 边界一致）。
//! 后续从单文件切换到目录结构时，只需调整 core 内部实现，调用方无需改动。

use std::path::{Path, PathBuf};

/// Core 存储根路径的类型化句柄。
///
/// 由调用方构造后传入 [`crate::core::TiangongCoreBuilder::storage`]。
/// core 通过 [`CoreStorageLocation::into_root`] 取出 `PathBuf` 传给 worker/engine。
#[derive(Debug, Clone)]
pub struct CoreStorageLocation {
    root: PathBuf,
}

impl CoreStorageLocation {
    /// 从路径创建存储位置。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 存储根路径。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 取出底层 `PathBuf`（消耗自身）。
    pub fn into_root(self) -> PathBuf {
        self.root
    }
}

impl From<PathBuf> for CoreStorageLocation {
    fn from(root: PathBuf) -> Self {
        Self { root }
    }
}
