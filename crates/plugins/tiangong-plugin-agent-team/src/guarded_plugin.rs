//! 子 Core 插件包装：在不扩展 Core Plugin trait 的前提下执行团队写入策略。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::{PermissionLevel, TrustModeHandle};
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

use crate::coordinator::Coordinator;
use crate::tools::error_result;

pub(crate) struct GuardedChildPlugin {
    inner: Arc<dyn Plugin>,
    coordinator: Weak<Coordinator>,
}

impl GuardedChildPlugin {
    pub(crate) fn new(inner: Arc<dyn Plugin>, coordinator: Weak<Coordinator>) -> Self {
        Self { inner, coordinator }
    }
}

impl ToolSpecProvider for GuardedChildPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.inner.tool_specs()
    }
}

impl ToolOverrideHandler for GuardedChildPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let Some(coordinator) = self.coordinator.upgrade() else {
            let result = error_result(&call.name, "所属 Agent Team 已关闭");
            return Box::pin(async move { Some(result) });
        };
        if let Err(reason) = coordinator.guard_child_tool_call(actor_id, call, session) {
            let result = error_result(&call.name, reason);
            return Box::pin(async move { Some(result) });
        }
        self.inner.handle(call, session, actor_id)
    }
}

impl PromptSectionProvider for GuardedChildPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        self.inner.prompt_sections()
    }
}

impl Plugin for GuardedChildPlugin {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn register(&self, engine: &RuntimeEngine) {
        self.inner.register(engine);
    }

    fn set_workspace(&self, workspace: Option<&Path>) {
        self.inner.set_workspace(workspace);
    }

    fn set_trust_mode(&self, trust: TrustModeHandle) {
        self.inner.set_trust_mode(trust);
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        self.inner.set_feedback_tx(tx);
    }

    fn collect_exec_env(&self) -> std::collections::BTreeMap<String, String> {
        self.inner.collect_exec_env()
    }

    fn allowed_file_roots(&self) -> Vec<PathBuf> {
        self.inner.allowed_file_roots()
    }

    fn tool_permission_overrides(&self) -> std::collections::BTreeMap<String, PermissionLevel> {
        self.inner.tool_permission_overrides()
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.inner.shutdown()
    }

    fn on_config_updated(&self, config: &tiangong_core::core_config::CoreConfig) {
        self.inner.on_config_updated(config);
    }

    fn on_session_ready(&self, session: &mut Session) {
        self.inner.on_session_ready(session);
    }

    fn on_engine_rebuilt(&self, session: &mut Session) {
        self.inner.on_engine_rebuilt(session);
    }

    fn on_cwd_changed(&self, session: &mut Session) {
        self.inner.on_cwd_changed(session);
    }

    fn on_turn_started(&self, session: &mut Session, turn_start_idx: usize) {
        self.inner.on_turn_started(session, turn_start_idx);
    }

    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        self.inner.on_turn_finished(session, turn_start_idx);
    }

    fn on_session_ended(&self, session: &mut Session) {
        self.inner.on_session_ended(session);
    }
}

pub(crate) fn child_write_targets(
    call: &ToolCall,
    workspace: &Path,
) -> Result<Vec<PathBuf>, String> {
    match call.name.as_str() {
        "write_file" | "replace_in_file" => {
            let raw = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{} 缺少 path 参数", call.name))?;
            Ok(vec![resolve_workspace_write_path(raw, workspace)?])
        }
        "apply_patch"
            if !call
                .arguments
                .get("verify")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false) =>
        {
            let patch = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "apply_patch 缺少 patch 参数".to_string())?;
            let workdir = call
                .arguments
                .get("workdir")
                .and_then(serde_json::Value::as_str);
            let cwd = restricted_cwd(workdir, workspace)?;
            unified_diff_targets(patch, &cwd)
        }
        "run_command" | "run_shell" => {
            let cwd = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str);
            let cwd = restricted_cwd(cwd, workspace)?;
            let root = canonical_workspace(workspace)?;
            let mut targets = vec![root, cwd];
            targets.sort();
            targets.dedup();
            Ok(targets)
        }
        "spawn_task" => Err("子 Agent 不能启动脱离当前轮次的后台任务".to_string()),
        _ => Ok(Vec::new()),
    }
}

fn canonical_workspace(workspace: &Path) -> Result<PathBuf, String> {
    workspace
        .canonicalize()
        .map_err(|error| format!("解析子 Agent 工作区失败：{error}"))
}

fn restricted_cwd(raw: Option<&str>, workspace: &Path) -> Result<PathBuf, String> {
    let root = canonical_workspace(workspace)?;
    let cwd = tiangong_toolkit::resolve_effective_cwd_with(raw, &root)
        .map_err(|error| format!("解析子 Agent 工作目录失败：{error}"))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    if cwd.starts_with(&root) {
        Ok(cwd)
    } else {
        Err(format!("子 Agent 只能在工作区内执行：{}", cwd.display()))
    }
}

fn resolve_workspace_write_path(raw: &str, workspace: &Path) -> Result<PathBuf, String> {
    let root = canonical_workspace(workspace)?;
    let target = tiangong_toolkit::resolve_write_path_from_base(raw, &root)
        .map_err(|error| format!("解析子 Agent 写入路径失败：{error}"))?;
    if target.starts_with(&root) {
        Ok(target)
    } else {
        Err(format!("子 Agent 只能写入工作区：{}", target.display()))
    }
}

fn unified_diff_targets(patch: &str, workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let lines = patch.lines().collect::<Vec<_>>();
    let mut targets = Vec::new();
    for pair in lines.windows(2) {
        if !pair[0].starts_with("--- ") || !pair[1].starts_with("+++ ") {
            continue;
        }
        for raw in [
            pair[0].trim_start_matches("--- "),
            pair[1].trim_start_matches("+++ "),
        ] {
            let raw = raw.split('\t').next().unwrap_or(raw).trim();
            if raw == "/dev/null" {
                continue;
            }
            let raw = raw
                .strip_prefix("a/")
                .or_else(|| raw.strip_prefix("b/"))
                .unwrap_or(raw);
            let target = resolve_workspace_write_path(raw, workspace)?;
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    if targets.is_empty() {
        return Err("apply_patch 未包含可识别的写入文件头".to_string());
    }
    Ok(targets)
}
