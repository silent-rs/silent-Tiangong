//! command 插件：持有会话工作目录、信任模式与各插件贡献的环境变量引用。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::permission::{TrustMode, TrustModeHandle};

/// command 插件。
pub struct CommandPlugin {
    workspace: RwLock<Option<PathBuf>>,
    trust_mode: RwLock<Option<TrustModeHandle>>,
    /// 各插件贡献的汇总环境变量（由 core 经 set_exec_env 回注）。
    ///
    /// core 在「所有插件注册完成后」汇总各插件的 `exec_env` 回注，
    /// command 执行子进程时读取即为最新值，无需 snapshot 刷新。
    runtime_env: RwLock<BTreeMap<String, String>>,
    /// 用户自定义的允许命令列表（扩展内置白名单）。
    ///
    /// 由 core 在 engine rebuild 时通过 `on_config_updated` 从 `CoreConfig` 注入。
    /// 校验时与内置白名单合并判断，白名单内命令免审批，白名单外命令走审批。
    allowed_commands: RwLock<Vec<String>>,
}

impl Default for CommandPlugin {
    fn default() -> Self {
        Self {
            workspace: RwLock::new(None),
            trust_mode: RwLock::new(None),
            runtime_env: RwLock::new(BTreeMap::new()),
            allowed_commands: RwLock::new(Vec::new()),
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

    /// 读取当前环境变量快照（从回注的汇总值取最新值）。
    pub(crate) fn runtime_env(&self) -> BTreeMap<String, String> {
        self.runtime_env
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub(crate) fn is_full_trust(&self) -> bool {
        let Ok(handle) = self.trust_mode.read() else {
            return false;
        };
        let Some(tm) = handle.as_ref() else {
            return false;
        };
        tm.current() == TrustMode::FullTrust
    }

    /// 读取用户自定义允许命令列表快照。
    pub(crate) fn allowed_commands(&self) -> Vec<String> {
        self.allowed_commands
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
impl CommandPlugin {
    /// 测试辅助：直接设置 allowed_commands（绕过 on_config_updated）。
    pub fn set_allowed_commands_for_test(&self, commands: Vec<String>) {
        if let Ok(mut guard) = self.allowed_commands.write() {
            *guard = commands;
        }
    }
}

impl Plugin for CommandPlugin {
    fn id(&self) -> &str {
        "command"
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

    fn set_exec_env(&self, env: BTreeMap<String, String>) {
        // 接收 core 汇总后的全部插件环境变量，run_command/run_shell 执行子进程时注入。
        if let Ok(mut guard) = self.runtime_env.write() {
            *guard = env;
        }
    }
}

impl tiangong_core::tool_override::PromptSectionProvider for CommandPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        // 原 core rules 第 8 条（run_command 部分）：命令执行默认走 run_command。
        vec!["命令执行默认使用 run_command，并根据工具结果继续推进。".to_string()]
    }
}
