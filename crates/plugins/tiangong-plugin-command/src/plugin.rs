//! command 插件：持有会话工作目录、信任模式与各插件贡献的环境变量引用。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tiangong_core::core::Plugin;
use tiangong_core::permission::TrustMode;

/// command 插件。
pub struct CommandPlugin {
    workspace: RwLock<Option<PathBuf>>,
    trust_mode: RwLock<Option<Arc<RwLock<TrustMode>>>>,
    /// 各插件贡献的环境变量共享句柄（register 时从 engine 获取）。
    ///
    /// core 在「所有插件注册完成后」汇总各插件的 `collect_exec_env` 写入同一句柄，
    /// command 执行子进程时读取即为最新值，无需 snapshot 刷新。
    runtime_env: RwLock<Arc<Mutex<BTreeMap<String, String>>>>,
}

impl Default for CommandPlugin {
    fn default() -> Self {
        Self {
            workspace: RwLock::new(None),
            trust_mode: RwLock::new(None),
            runtime_env: RwLock::new(Arc::new(Mutex::new(BTreeMap::new()))),
        }
    }
}

impl CommandPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    /// 读取当前环境变量快照（从共享句柄取最新值）。
    pub(crate) fn runtime_env(&self) -> BTreeMap<String, String> {
        let handle = self
            .runtime_env
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| Arc::new(Mutex::new(BTreeMap::new())));
        handle.lock().map(|g| g.clone()).unwrap_or_default()
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
        // 持有 engine 的 runtime_env 共享句柄——core 在所有插件注册完成后会汇总
        // 各插件的 collect_exec_env 写入同一句柄，此处读取即为最新值。
        if let Ok(mut guard) = self.runtime_env.write() {
            *guard = engine.runtime_env_handle();
        }
    }
}

impl tiangong_core::tool_override::PromptSectionProvider for CommandPlugin {}
