//! command 插件：持有会话工作目录、信任模式与 MCP/skills 环境变量引用。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tiangong_core::core::plugin::{Plugin, check_full_trust};
use tiangong_core::permission::TrustMode;

/// command 插件。
#[derive(Default)]
pub struct CommandPlugin {
    workspace: RwLock<Option<PathBuf>>,
    trust_mode: RwLock<Option<Arc<RwLock<TrustMode>>>>,
    /// MCP/skills 收集的环境变量（register 时从 engine 获取，注入子进程）。
    runtime_env: RwLock<BTreeMap<String, String>>,
}

impl CommandPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    pub(crate) fn runtime_env(&self) -> BTreeMap<String, String> {
        self.runtime_env
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub(crate) fn is_full_trust(&self) -> bool {
        self.trust_mode
            .read()
            .map(|g| check_full_trust(g.as_ref()))
            .unwrap_or(false)
    }
}

impl Plugin for CommandPlugin {
    fn id(&self) -> &str {
        "command"
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

    fn register(&self, engine: &tiangong_core::runtime::RuntimeEngine) {
        // 信任模式已由 core 通过 set_trust_mode 统一注入，此处仅获取 MCP/skills 收集的
        // 环境变量快照（子进程执行时注入）
        if let Ok(mut guard) = self.runtime_env.write() {
            *guard = engine.runtime_env();
        }
    }
}

impl tiangong_core::tool_override::PromptSectionProvider for CommandPlugin {}
