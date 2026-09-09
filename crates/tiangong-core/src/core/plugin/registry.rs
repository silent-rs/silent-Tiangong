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
    refresh_from_plugins(sorted)
}

/// 每轮刷新声明：实时收集全部插件声明。tools 顺序与 prompt 内容的
/// 稳定（含读取失败的兜底）由插件自身负责；读取失败的插件本轮缺席，
/// 恢复后下一轮自动回归。插件集合变化（启停/安装/移除）即时反映。
pub(crate) fn refresh_from_plugins(plugins: Vec<Arc<dyn Plugin>>) -> PreparedPlugins {
    let declarations = collect_declarations(&plugins);
    PreparedPlugins::restore(plugins, declarations)
}

fn collect_declarations(plugins: &[Arc<dyn Plugin>]) -> Vec<PluginDeclaration> {
    // Core 自带的反馈工具也保存完整定义，与插件声明一同参与 restore。
    let mut declarations = vec![PluginDeclaration {
        plugin_id: String::new(),
        tools: vec![injection_tool_spec()],
        prompt_sections: Vec::new(),
    }];
    for plugin in plugins {
        match plugin.try_tool_specs().and_then(|tools| {
            plugin
                .try_prompt_sections()
                .map(|prompt_sections| PluginDeclaration {
                    plugin_id: plugin.id().to_string(),
                    tools,
                    prompt_sections,
                })
        }) {
            Ok(declaration) => declarations.push(declaration),
            Err(error) => tracing::warn!(
                plugin_id = plugin.id(),
                %error,
                "声明读取失败，本轮缺席（插件恢复后自动回归）"
            ),
        }
    }
    declarations
}

impl PreparedPlugins {
    pub(crate) fn restore(
        plugins: Vec<Arc<dyn Plugin>>,
        declarations: Vec<PluginDeclaration>,
    ) -> Self {
        let mut tools = Vec::new();
        let mut tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        let mut seen_tool_names = HashSet::new();
        let mut prompt_sections = Vec::new();
        for declaration in &declarations {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler,
    };

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    struct OrderedPlugin {
        id: String,
        fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
        names: &'static [&'static str],
    }
    impl Plugin for OrderedPlugin {
        fn id(&self) -> &str {
            &self.id
        }
    }
    impl crate::tool_override::ToolSpecProvider for OrderedPlugin {
        fn try_tool_specs(&self) -> Result<Vec<ToolSpec>, String> {
            if self.fail.load(std::sync::atomic::Ordering::Acquire) {
                Err("声明读取失败".to_string())
            } else {
                Ok(self.names.iter().map(|name| tool(name)).collect())
            }
        }
    }
    impl PromptSectionProvider for OrderedPlugin {}
    impl ToolOverrideHandler for OrderedPlugin {}
    impl MentionCandidateProvider for OrderedPlugin {}

    /// 顺序与缺席语义的行为锁定：tools 顺序为内置注入工具、插件传入序、
    /// 插件自身输出序三者叠加（core 不排序）；读取失败的插件本轮缺席，
    /// 插件移除后其工具即时消失。
    #[test]
    fn declarations_keep_plugin_order_and_skip_failures() {
        let marker = format!("order-{}", line!());
        let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(OrderedPlugin {
                id: format!("{marker}-zeta"),
                fail: fail.clone(),
                names: &["z_b_first", "a_second"],
            }),
            Arc::new(OrderedPlugin {
                id: format!("{marker}-alpha"),
                fail: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                names: &["alpha_tool"],
            }),
        ];

        // 首次收集：保持插件输出顺序，tools 不按名称重排。
        let prepared = refresh_from_plugins(plugins.clone());
        let names: Vec<&str> = prepared.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["plugin_injection", "z_b_first", "a_second", "alpha_tool"],
            "tools 顺序应为：内置注入工具 + 插件传入序 + 插件自身输出序"
        );

        // 声明读取失败：该插件本轮缺席（兜底由插件自身负责）。
        fail.store(true, std::sync::atomic::Ordering::Release);
        let refreshed = refresh_from_plugins(plugins.clone());
        let names: Vec<&str> = refreshed.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["plugin_injection", "alpha_tool"],
            "读取失败的插件应缺席"
        );

        // 恢复后下一轮自动回归；插件移除后其工具即时消失。
        fail.store(false, std::sync::atomic::Ordering::Release);
        let recovered = refresh_from_plugins(plugins.clone());
        let names: Vec<&str> = recovered.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["plugin_injection", "z_b_first", "a_second", "alpha_tool"]
        );
        let only_alpha = refresh_from_plugins(vec![plugins[1].clone()]);
        let names: Vec<&str> = only_alpha.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["plugin_injection", "alpha_tool"]);
    }
}
