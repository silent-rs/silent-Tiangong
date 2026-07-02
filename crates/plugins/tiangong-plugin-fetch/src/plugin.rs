//! web_fetch 插件：持有会话工作目录与信任模式引用。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tiangong_core::core::Plugin;
use tiangong_core::permission::TrustMode;

/// web_fetch 插件。
#[derive(Default)]
pub struct FetchPlugin {
    workspace: RwLock<Option<PathBuf>>,
    trust_mode: RwLock<Option<Arc<RwLock<TrustMode>>>>,
}

impl FetchPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    pub(crate) fn is_full_trust(&self) -> bool {
        let Ok(handle) = self.trust_mode.read() else {
            return false;
        };
        let Some(tm) = handle.as_ref() else {
            return false;
        };
        tm.read()
            .map(|g| *g == TrustMode::FullTrust)
            .unwrap_or(false)
    }
}

impl Plugin for FetchPlugin {
    fn id(&self) -> &str {
        "fetch"
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
        }
    }

    fn set_trust_mode(&self, trust: Arc<RwLock<TrustMode>>) {
        if let Ok(mut guard) = self.trust_mode.write() {
            *guard = Some(trust);
        }
    }

    // register 留空：信任模式已由 core 通过 set_trust_mode 统一注入。
}

impl tiangong_core::tool_override::PromptSectionProvider for FetchPlugin {}
