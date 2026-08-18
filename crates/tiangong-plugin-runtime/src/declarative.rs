//! 声明式插件适配器：把纯 UI 插件 manifest 的宿主服务工具声明与 prompt
//! 段落包装为 Core [`Plugin`]，经现有 trait 通道统一注册（WASM Runtime 边界）。
//!
//! 声明归插件、执行归宿主：工具 spec/prompt 走标准收集；`host_handler`
//! 白名单工具的执行在 [`ToolOverrideHandler::handle`] 内编排——创建交互请求、
//! 发事件、等待注册表原子闭合（与等待 LLM 同为工具执行的外部 IO），
//! 审批授权由 Core 侧交互模块按挑战真实目标生成。插件无权自定义执行体。

use std::sync::{Arc, Mutex};

use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::ToolOverrideHandler;

/// 为启用插件的 manifest 声明构建声明式插件列表。
///
/// 仅包含声明了 `tools[]` 或 `prompt[]` 的插件（schema v2）；同名工具
/// 先到先得（重名声明跳过并告警）。
pub fn declarative_plugins() -> Vec<Arc<dyn Plugin>> {
    let Some(declared) = crate::registry::declared_tools_and_prompts() else {
        return Vec::new();
    };
    let mut taken_names = std::collections::BTreeSet::new();
    let mut result: Vec<Arc<dyn Plugin>> = Vec::new();
    for item in declared {
        let mut specs = Vec::with_capacity(item.tools.len());
        let mut handlers = std::collections::BTreeMap::new();
        for tool in &item.tools {
            if !taken_names.insert(tool.name.clone()) {
                tracing::warn!(
                    plugin_id = %item.plugin_id,
                    tool = %tool.name,
                    "宿主服务工具重名，后声明者被跳过"
                );
                continue;
            }
            handlers.insert(tool.name.clone(), tool.host_handler.clone());
            specs.push(ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            });
        }
        if specs.is_empty() && item.prompts.is_empty() {
            continue;
        }
        result.push(Arc::new(DeclarativePlugin {
            plugin_id: item.plugin_id,
            specs,
            prompts: item.prompts,
            handlers,
            feedback: Mutex::new(None),
        }));
    }
    result
}

/// 单个插件声明的快照（来自注册表）。
pub struct DeclaredPlugin {
    pub plugin_id: String,
    pub tools: Vec<crate::manifest::HostToolDecl>,
    pub prompts: Vec<String>,
}

/// 声明式插件：工具规格与 prompt 来自 manifest；宿主服务工具的执行经
/// `host_handler` 白名单路由（当前仅 `interaction.request_user`）。
struct DeclarativePlugin {
    plugin_id: String,
    specs: Vec<ToolSpec>,
    prompts: Vec<String>,
    handlers: std::collections::BTreeMap<String, String>,
    feedback: Mutex<Option<PluginFeedbackTx>>,
}

impl Plugin for DeclarativePlugin {
    fn id(&self) -> &str {
        &self.plugin_id
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        *self.feedback.lock().expect("声明式插件反馈锁损坏") = Some(tx);
    }
}

impl tiangong_core::tool_override::ToolSpecProvider for DeclarativePlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }
}

impl tiangong_core::tool_override::PromptSectionProvider for DeclarativePlugin {
    fn prompt_sections(&self) -> Vec<String> {
        self.prompts.clone()
    }
}

impl tiangong_core::tool_override::MentionCandidateProvider for DeclarativePlugin {}

impl ToolOverrideHandler for DeclarativePlugin {
    fn is_host_service_tool(&self) -> bool {
        true
    }

    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let handler = self.handlers.get(&call.name).cloned();
        let arguments = call.arguments.clone();
        let call_id = call.id.clone();
        let call_name = call.name.clone();
        let session_id = session.id.clone();
        let feedback = self.feedback.lock().expect("声明式插件反馈锁损坏").clone();
        Box::pin(async move {
            let Some(handler) = handler else {
                return None; // 非本插件声明的宿主工具：不拦截
            };
            if handler != "interaction.request_user" {
                tracing::warn!(tool = %call_name, handler = %handler, "未知宿主服务处理器");
                return Some(ToolResult {
                    ok: false,
                    summary: format!("宿主服务处理器 {handler} 未实现"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 1,
                    execution: None,
                });
            }
            // 事件经本轮反馈通道送达（未注入时降级：请求仍创建，界面无实时事件）
            match tiangong_core::interaction::run_request_user(
                &session_id,
                &call_id,
                &arguments,
                feedback.as_ref(),
            )
            .await
            {
                Ok((payload, ok)) => Some(ToolResult {
                    ok,
                    summary: payload,
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: if ok { 0 } else { 1 },
                    execution: None,
                }),
                Err(message) => Some(ToolResult {
                    ok: false,
                    summary: format!("request_user 参数无效：{message}"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 1,
                    execution: None,
                }),
            }
        })
    }
}
