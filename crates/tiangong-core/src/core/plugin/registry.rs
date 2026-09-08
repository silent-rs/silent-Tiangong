use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::core_config::CoreConfig;
use crate::model::ToolSpec;
use crate::permission::TrustMode;
use crate::session::{PluginDeclaration, Session};
use crate::tool_override::ToolOverrideHandler;

use super::{Plugin, injection_tool_spec};

#[derive(Clone, Default)]
pub struct PreparedPlugins {
    pub plugins: Vec<Arc<dyn Plugin>>,
    pub tools: Vec<ToolSpec>,
    pub tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
    pub prompt_sections: Vec<String>,
}

pub(crate) fn prepare_plugins(
    plugins: &[Arc<dyn Plugin>],
    config: &CoreConfig,
    trust_mode: TrustMode,
    session: &mut Session,
) -> PreparedPlugins {
    // 提示与工具共用固定顺序，避免加载顺序变化改写请求前缀。
    let mut sorted: Vec<Arc<dyn Plugin>> = plugins.to_vec();
    sorted.sort_by(|left, right| {
        (left.id() != "prompt")
            .cmp(&(right.id() != "prompt"))
            .then_with(|| left.id().cmp(right.id()))
    });

    let plugins = sorted.as_slice();
    let workspace_path = std::path::Path::new(&session.cwd);
    let workspace = workspace_path.is_dir().then_some(workspace_path);

    // 先汇总 exec_env，使依赖 sidecar 的插件能在首次生命周期调用触发启动前
    // 把受控环境注入 sidecar 进程。
    let mut exec_env = BTreeMap::new();
    for plugin in plugins {
        for (key, value) in plugin.exec_env() {
            exec_env.insert(key, value);
        }
    }
    for plugin in plugins {
        plugin.set_exec_env(exec_env.clone());
    }
    for plugin in plugins {
        plugin.on_config_updated(config);
        plugin.set_workspace(workspace);
        plugin.set_trust_mode(trust_mode);
    }
    for plugin in plugins {
        plugin.on_session_ready(session);
    }
    if session.plugin_declarations.is_none() {
        session.plugin_declarations = Some(collect_declarations(plugins, &[]));
    }
    PreparedPlugins::restore(sorted, session.plugin_declarations.as_deref().unwrap())
}

pub(crate) fn collect_declarations(
    plugins: &[Arc<dyn Plugin>],
    previous: &[PluginDeclaration],
) -> Vec<PluginDeclaration> {
    // Core 自带的反馈工具也保存完整定义，避免升级后恢复时悄悄改写 tools。
    let mut declarations = vec![PluginDeclaration {
        plugin_id: String::new(),
        tools: vec![injection_tool_spec()],
        prompt_sections: Vec::new(),
    }];
    for plugin in plugins {
        let result = plugin.try_tool_specs().and_then(|tools| {
            plugin
                .try_prompt_sections()
                .map(|prompt_sections| PluginDeclaration {
                    plugin_id: plugin.id().to_string(),
                    tools,
                    prompt_sections,
                })
        });
        let mut declaration = match result {
            Ok(declaration) => declaration,
            Err(error) => {
                let Some(previous) = previous.iter().find(|item| item.plugin_id == plugin.id())
                else {
                    tracing::warn!(plugin_id = plugin.id(), %error, "声明读取失败且没有历史定义，暂不加入该插件声明");
                    continue;
                };
                tracing::warn!(plugin_id = plugin.id(), %error, "声明读取失败，保留会话原声明");
                previous.clone()
            }
        };
        declaration
            .tools
            .sort_by(|left, right| left.name.cmp(&right.name));
        declarations.push(declaration);
    }
    declarations
}

impl PreparedPlugins {
    pub(crate) fn restore(
        plugins: Vec<Arc<dyn Plugin>>,
        declarations: &[PluginDeclaration],
    ) -> Self {
        let mut tools = Vec::new();
        let mut tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        let mut seen_tool_names = HashSet::new();
        let mut prompt_sections = Vec::new();
        for declaration in declarations {
            prompt_sections.extend(declaration.prompt_sections.clone());
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.id() == declaration.plugin_id);
            let plugin_tools = declaration.tools.clone();
            for spec in plugin_tools {
                if seen_tool_names.insert(spec.name.clone()) {
                    if let Some(plugin) = plugin {
                        tool_overrides.insert(spec.name.clone(), plugin.clone());
                    }
                    tools.push(spec);
                } else {
                    tracing::debug!(
                        tool = %spec.name,
                        plugin = %declaration.plugin_id,
                        "跳过与其他插件重名的工具规格（保留先注册者）"
                    );
                }
            }
        }

        PreparedPlugins {
            plugins,
            tools,
            tool_overrides,
            prompt_sections,
        }
    }
}
