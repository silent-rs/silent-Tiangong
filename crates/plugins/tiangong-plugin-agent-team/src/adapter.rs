//! 父 Core 中的 Agent Team 插件适配层。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tiangong_core::core::command::Command;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::{PermissionLevel, TrustModeHandle};
use tiangong_core::session::{PendingPluginDelivery, Session};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
use tiangong_types::ContentBlock;

use crate::child_runtime::ChildPluginFactory;
use crate::constants::{PLUGIN_ID, TOOL_CREATE_AGENT};
use crate::coordinator::Coordinator;
use crate::tools::{error_result, root_tool_specs};

pub struct AgentTeamPlugin {
    coordinator: Arc<Coordinator>,
    feedback: RwLock<Option<PluginFeedbackTx>>,
    trust_mode: RwLock<Option<TrustModeHandle>>,
}

impl AgentTeamPlugin {
    pub fn new(storage_root: PathBuf, child_plugins: Arc<dyn ChildPluginFactory>) -> Self {
        Self {
            coordinator: Coordinator::new(storage_root, child_plugins),
            feedback: RwLock::new(None),
            trust_mode: RwLock::new(None),
        }
    }

    fn feedback(&self) -> Option<PluginFeedbackTx> {
        self.feedback
            .read()
            .ok()
            .and_then(|feedback| feedback.clone())
    }

    fn current_trust_mode(&self) -> tiangong_core::permission::TrustMode {
        self.trust_mode
            .read()
            .ok()
            .and_then(|trust| trust.as_ref().map(TrustModeHandle::current))
            .unwrap_or_default()
    }
}

impl ToolSpecProvider for AgentTeamPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        root_tool_specs()
    }
}

impl ToolOverrideHandler for AgentTeamPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        if !root_tool_specs().iter().any(|spec| spec.name == call.name) {
            return Box::pin(async { None });
        }
        if call.name == TOOL_CREATE_AGENT {
            let result = self.coordinator.create_agent(call, session);
            return Box::pin(async move { Some(result) });
        }
        let coordinator = Arc::clone(&self.coordinator);
        let call = call.clone();
        let actor_id = actor_id.to_string();
        let feedback = self.feedback().map(PluginFeedbackTx::for_current_turn);
        let trust_mode = self.current_trust_mode();
        Box::pin(async move {
            let Some(feedback) = feedback else {
                return Some(error_result(&call.name, "Agent Team 反馈通道不可用"));
            };
            Some(
                coordinator
                    .handle_tool(call, actor_id, feedback, trust_mode)
                    .await,
            )
        })
    }
}

impl PromptSectionProvider for AgentTeamPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![format!(
            "团队协作能力（可选）：\n\
             - create_agent 创建由独立 TiangongCore 承载的成员，最多 8 个；成员使用与当前 Core 相同的插件能力。\n\
             - send_message 会投递到目标子 Core，并等待其外部 Done/Error 终态后返回。\n\
             - 用户输入中的 @role / @all 会由插件直接可靠投递，不要再次调用 send_message。\n\
             - 子 Agent 向 main 只能异步报告；同级等待只允许沿创建顺序向后。\n\
             - 子 Agent 修改文件前必须加锁，命令必须前台、有限时。\n{}",
            self.coordinator.roster_prompt()
        )]
    }
}

impl Plugin for AgentTeamPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn set_workspace(&self, workspace: Option<&Path>) {
        self.coordinator.set_workspace(workspace);
    }

    fn set_trust_mode(&self, trust_mode: TrustModeHandle) {
        if let Ok(mut current) = self.trust_mode.write() {
            *current = Some(trust_mode.clone());
        }
        self.coordinator.set_trust_mode(trust_mode);
    }

    fn set_feedback_tx(&self, feedback: PluginFeedbackTx) {
        if let Ok(mut current) = self.feedback.write() {
            *current = Some(feedback.clone());
        }
        self.coordinator.set_feedback(feedback);
    }

    fn on_config_updated(&self, config: &tiangong_core::core_config::CoreConfig) {
        self.coordinator.update_config(config);
    }

    fn on_session_ready(&self, session: &mut Session) {
        self.coordinator.initialize(session);
    }

    fn plan_plugin_deliveries(
        &self,
        actor_id: &str,
        source_message_id: &str,
        prepared: &[ContentBlock],
    ) -> Vec<PendingPluginDelivery> {
        self.coordinator
            .plan_deliveries(actor_id, source_message_id, prepared)
    }

    fn dispatch_plugin_deliveries(&self, session: &Session, source_message_id: &str) -> bool {
        self.coordinator
            .dispatch_deliveries(session, source_message_id, self.current_trust_mode())
    }

    fn handle_runtime_command(&self, command: &Command) -> bool {
        self.coordinator.handle_runtime_command(command)
    }

    fn shutdown<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move { self.coordinator.shutdown().await })
    }

    fn tool_permission_overrides(&self) -> std::collections::BTreeMap<String, PermissionLevel> {
        root_tool_specs()
            .into_iter()
            .map(|spec| (spec.name, PermissionLevel::Safe))
            .collect()
    }
}
