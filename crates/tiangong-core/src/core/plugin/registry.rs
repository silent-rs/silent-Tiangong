use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::core_config::CoreConfig;
use crate::model::ToolSpec;
use crate::permission::TrustMode;
use crate::session::Session;
use crate::tool_override::ToolOverrideHandler;

use super::{Plugin, injection_tool_spec};

pub(crate) struct PreparedPlugins {
    pub tools: Vec<ToolSpec>,
    pub tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
}

pub(crate) fn prepare_plugins(
    plugins: &[Arc<dyn Plugin>],
    config: &CoreConfig,
    trust_mode: TrustMode,
    session: &Session,
) -> PreparedPlugins {
    let prepare_start = std::time::Instant::now();
    let workspace_path = std::path::Path::new(&session.cwd);
    let workspace = workspace_path.is_dir().then_some(workspace_path);

    for plugin in plugins {
        plugin.on_config_updated(config);
        plugin.set_workspace(workspace);
        plugin.set_trust_mode(trust_mode);
    }
    tracing::info!(
        plugin_count = plugins.len(),
        elapsed_ms = prepare_start.elapsed().as_millis() as u64,
        "prepare_plugins 完成（每轮 turn 调用一次）"
    );

    let mut exec_env = BTreeMap::new();
    for plugin in plugins {
        for (key, value) in plugin.exec_env() {
            exec_env.insert(key, value);
        }
    }
    for plugin in plugins {
        plugin.set_exec_env(exec_env.clone());
    }

    let mut tools = vec![injection_tool_spec()];
    let mut tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
    let mut seen_tool_names = HashSet::new();
    for plugin in plugins {
        for spec in plugin.tool_specs() {
            if seen_tool_names.insert(spec.name.clone()) {
                tool_overrides.insert(spec.name.clone(), plugin.clone());
                tools.push(spec);
            } else {
                tracing::debug!(
                    tool = %spec.name,
                    plugin = %plugin.id(),
                    "跳过与其他插件重名的工具规格（保留先注册者）"
                );
            }
        }
    }

    PreparedPlugins {
        tools,
        tool_overrides,
    }
}
