//! 基础文件工具插件：聚合工具规格与覆盖处理器。
//!
//! [`FsPlugin`] 通过 [`Plugin::set_workspace`] 接收 core 注入的会话工作目录，
//! 通过 [`Plugin::set_trust_mode`] 接收 core 注入的会话信任解析句柄。工具规格与
//! 覆盖处理器直接在本类型上实现，core 通过 supertrait 自动收集。

use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::permission::{TrustMode, TrustModeHandle};

/// 基础文件工具插件。
///
/// `workspace` 由 core 在 engine 创建及每次会话目录变更时注入（可变）；
/// `trust_mode` 由 core 在 register 前通过 `set_trust_mode` 注入（共享会话基线）。
#[derive(Default)]
pub struct FsPlugin {
    /// 当前会话工作目录（可变，由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 信任模式解析句柄（set_trust_mode 时注入，FullTrust 时放宽路径校验）。
    trust_mode: RwLock<Option<TrustModeHandle>>,
}

impl FsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前工作目录的快照。
    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    /// 当前是否处于完全信任模式。
    pub(crate) fn is_full_trust(&self) -> bool {
        let Ok(handle) = self.trust_mode.read() else {
            return false;
        };
        let Some(tm) = handle.as_ref() else {
            return false;
        };
        tm.current() == TrustMode::FullTrust
    }
}

impl Plugin for FsPlugin {
    fn id(&self) -> &str {
        "fs"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn set_trust_mode(&self, trust: TrustModeHandle) {
        if let Ok(mut guard) = self.trust_mode.write() {
            *guard = Some(trust);
        }
    }

    // register 留空：信任模式已由 core 通过 set_trust_mode 统一注入，
    // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集。
}

// PromptSectionProvider 使用默认空实现（fs 不注入 prompt 段落）
impl tiangong_core::tool_override::PromptSectionProvider for FsPlugin {}
