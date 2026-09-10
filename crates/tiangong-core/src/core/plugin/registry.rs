use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::core_config::CoreConfig;
use crate::model::ToolSpec;
use crate::permission::TrustMode;
use crate::session::Session;
use crate::tool_override::ToolOverrideHandler;

use super::{Plugin, injection_tool_spec};

pub(crate) struct PreparedPlugins {
    pub plugins: Vec<Arc<dyn Plugin>>,
    pub tools: Vec<ToolSpec>,
    pub tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
}

/// 每轮 turn 构建上下文时调用：排序注入生命周期钩子并收集工具声明。
///
/// core 不做任何声明稳定化处理——运行期稳定由 runtime 适配器的冻结
/// 快照保证；工具顺序即插件输出顺序，core 不代为排序。声明读取失败
/// 由适配器降级为空列表，core 不感知。
pub(crate) fn prepare_plugins(
    plugins: &[Arc<dyn Plugin>],
    config: &CoreConfig,
    trust_mode: TrustMode,
    session: &Session,
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
        plugin.set_execution_context(workspace, trust_mode);
    }

    let mut tools = vec![injection_tool_spec()];
    let mut tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
    let mut seen_tool_names = HashSet::new();
    for plugin in plugins {
        let plugin_tools = plugin.tool_specs();
        for spec in plugin_tools {
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
        plugins: sorted,
        tools,
        tool_overrides,
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
        names: &'static [&'static str],
    }
    impl Plugin for OrderedPlugin {
        fn id(&self) -> &str {
            &self.id
        }
    }
    impl crate::tool_override::ToolSpecProvider for OrderedPlugin {
        fn tool_specs(&self) -> Vec<ToolSpec> {
            self.names.iter().map(|name| tool(name)).collect()
        }
    }
    impl PromptSectionProvider for OrderedPlugin {}
    impl ToolOverrideHandler for OrderedPlugin {}
    impl MentionCandidateProvider for OrderedPlugin {}

    /// 顺序语义锁定：tools 顺序 = 内置注入工具 + 插件 id 字典序（prompt
    /// 置顶）+ 插件自身输出序（core 不排序）；重名工具保留先注册者。
    #[test]
    fn prepare_keeps_plugin_order_and_dedupes() {
        let marker = format!("order-{}", line!());
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(OrderedPlugin {
                id: format!("{marker}-zeta"),
                names: &["z_b_first", "a_second"],
            }),
            Arc::new(OrderedPlugin {
                id: format!("{marker}-alpha"),
                names: &["alpha_tool", "z_b_first"],
            }),
        ];
        let session = Session::new("顺序");
        let prepared = prepare_plugins(
            &plugins,
            &CoreConfig::default(),
            TrustMode::default(),
            &session,
        );
        let names: Vec<&str> = prepared.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["plugin_injection", "alpha_tool", "z_b_first", "a_second"],
            "tools 顺序应为：内置注入工具 + 插件 id 序 + 插件输出序，重名保留先注册者"
        );
    }
}
