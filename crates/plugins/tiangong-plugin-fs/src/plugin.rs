//! 基础文件工具插件：聚合工具规格与覆盖处理器。
//!
//! [`FsPlugin`] 通过 [`Plugin::set_workspace`] 接收 core 注入的会话工作目录，
//! 在 [`Plugin::register`] 时从 engine 获取共享信任模式引用。工具规格与覆盖处理器
//! 直接在本类型上实现，core 通过 supertrait 自动收集。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tiangong_core::core::Plugin;
use tiangong_core::permission::TrustMode;

/// 基础文件工具插件。
///
/// `workspace` 由 core 在 engine 创建及每次会话目录变更时注入（可变）；
/// `trust_mode` 在 register 时从 engine 获取（共享引用，值实时同步）。
#[derive(Default)]
pub struct FsPlugin {
    /// 当前会话工作目录（可变，由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 共享信任模式引用（register 时注入，FullTrust 时放宽路径校验）。
    trust_mode: RwLock<Option<Arc<RwLock<TrustMode>>>>,
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
        let guard = match self.trust_mode.read() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let Some(trust) = guard.as_ref() else {
            return false;
        };
        trust
            .read()
            .map(|g| *g == TrustMode::FullTrust)
            .unwrap_or(false)
    }
}

impl Plugin for FsPlugin {
    fn id(&self) -> &str {
        "fs"
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn register(&self, engine: &tiangong_core::runtime::RuntimeEngine) {
        // 获取共享信任模式引用（与 LocalToolExecutor 内部那份指向同一个 RwLock）
        let trust = engine.permission_gate().shared_trust_mode_ref();
        if let Ok(mut guard) = self.trust_mode.write() {
            *guard = Some(trust);
        }
        // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集。
    }
}

// PromptSectionProvider 使用默认空实现（fs 不注入 prompt 段落）
impl tiangong_core::tool_override::PromptSectionProvider for FsPlugin {}
