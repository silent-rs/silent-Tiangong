//! 一轮对话的执行上下文。
//!
//! [`TurnContext`] 的生命周期严格限制为单个 turn：收到 Message 时由 typed builder
//! 构造,turn 结束后整体销毁。它持有 turn 执行所需的 client / 权限 / 工具 / 用量收集器。
//!
//! 与 `react/` 模块的关系:`TurnContext` 是被 react 层消费的能力集合,本身不属于
//! ReAct 执行流程。`react/turn.rs` 通过独立的 `execute_turn` 函数消费本结构。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::agent_config::AgentConfig;
use crate::core::plugin::Plugin;
use crate::model::{SingleProviderClient, ToolCall, ToolSpec};
use crate::session::Session;
use crate::tool::ToolResult;
use crate::tool_override::ToolOverrideHandler;
use tiangong_types::StreamEvent;

use typed_builder::TypedBuilder;

/// 一轮对话的执行上下文（替代原 ReactEngine + RuntimeEngine）。
///
/// 生命周期严格限制为单个 turn：收到 Message 时构造,
/// turn 结束后整体销毁。不跨 turn 复用。
#[derive(TypedBuilder)]
#[builder(
    builder_method(vis = "pub(crate)"),
    builder_type(vis = "pub(crate)"),
    build_method(vis = "pub(crate)")
)]
pub struct TurnContext {
    /// 模型请求客户端
    pub client: SingleProviderClient,
    /// 本轮会话（turn 期间独占,turn 结束时取回落盘）
    pub session: Session,
    /// 本轮内部事件发送端。
    pub stream_tx: Sender<StreamEvent>,
    /// 本轮使用的插件及生命周期钩子。
    pub plugins: Vec<Arc<dyn Plugin>>,
    /// 上下文 token 上限
    pub context_limit: usize,
    /// Agent 配置（reasoning_effort 等）
    pub agent_config: AgentConfig,
    /// 会话信任模式（FullTrust 放行一切,否则需审批;审批在 turn 层统一完成）
    pub trust_mode: crate::permission::TrustMode,
    /// 观测器（审计日志写入,持有 storage_root）
    pub observer: crate::observe::Observer,
    /// 构建前收集完成的工具覆盖处理器。
    tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
    /// Turn-scoped 插件 usage 收集器
    pub turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
    // ===== turn 级配置 =====
    /// 当前执行单元可用的工具集
    pub tools: Vec<ToolSpec>,
    /// 单次工具执行阶段（ReAct Loop 内层）的最大轮次
    #[builder(default = crate::MAX_TOOL_ROUNDS)]
    pub max_tool_rounds: usize,
    /// 总结阶段后重新进入工具执行阶段的最大次数
    #[builder(default = crate::MAX_OUTER_ITERATIONS)]
    pub max_outer_iterations: u32,
}

impl TurnContext {
    // ===== 能力 accessor =====

    pub fn client(&self) -> &SingleProviderClient {
        &self.client
    }

    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    pub fn turn_usage_sink(&self) -> &Arc<crate::core::plugin::TurnUsageSink> {
        &self.turn_usage_sink
    }

    // ===== 工具执行 =====

    /// 执行单个工具调用。
    ///
    /// 权限审批在 turn 层统一完成（turn.rs 的工具执行循环）;
    /// 到达此方法时审批已通过,handler 直接执行。
    pub(crate) fn start_tool_call(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        if let Some(handler) = self.tool_overrides.get(&call.name).cloned() {
            let handler_future = handler.handle(call, session, actor_id);
            let tool_name = call.name.clone();
            return Box::pin(async move {
                if let Some(result) = handler_future.await {
                    return result;
                }

                ToolResult {
                    ok: false,
                    summary: format!("未注册的工具：{tool_name}（请确认对应插件已启用）"),
                    stdout: String::new(),
                    stderr: format!("tool {tool_name} not handled by any plugin"),
                    exit_code: 1,
                    execution: None,
                }
            });
        }

        let tool_name = call.name.clone();
        Box::pin(async move {
            ToolResult {
                ok: false,
                summary: format!("未注册的工具：{tool_name}（请确认对应插件已启用）"),
                stdout: String::new(),
                stderr: format!("tool {tool_name} not handled by any plugin"),
                exit_code: 1,
                execution: None,
            }
        })
    }
}
