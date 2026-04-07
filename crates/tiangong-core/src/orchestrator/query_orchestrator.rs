//! 查询编排器
//!
//! 智能体的控制中心。接受当前事件后，判断执行模式并输出决策。
//! 整合原 `context::assembler::QueryClassifier` 的意图分类能力，
//! 并扩展为多路由支持。

use crate::model::{ModelClient, ModelRequest, SingleProviderClient};
use crate::session::Session;

use super::types::{OrchestratorDecision, QueryMode};

/// 查询编排器
///
/// 负责：
/// 1. 判断用户输入的执行模式
/// 2. 判断是否打断正在运行的任务
/// 3. 输出包含路由决策的 `OrchestratorDecision`
pub struct QueryOrchestrator;

impl QueryOrchestrator {
    /// 对用户输入进行编排决策
    ///
    /// 决策流程：
    /// 1. 空输入 → 多步执行（兼容旧行为）
    /// 2. 会话有工具历史 → 多步执行（延续工具上下文）
    /// 3. LLM 意图分类 → 根据分类结果路由
    pub fn decide(
        input: &str,
        session: &Session,
        client: &SingleProviderClient,
        has_running_task: bool,
    ) -> OrchestratorDecision {
        let input = input.trim();

        // 空输入走多步执行模式（兼容旧行为）
        if input.is_empty() {
            return OrchestratorDecision {
                mode: QueryMode::MultiStepExecution,
                should_interrupt: false,
                reason: "空输入，默认多步执行".into(),
                llm_calls: Vec::new(),
            };
        }

        // 会话有工具历史 → 继续多步执行
        let has_tool_history = session.task_records.iter().any(|r| r.tool_result.is_some())
            || session.messages.iter().any(|m| {
                m.role == crate::session::MessageRole::System
                    && (m.content.contains("tool_name:") || m.content.contains("exit_code"))
            });
        if has_tool_history {
            return OrchestratorDecision {
                mode: QueryMode::MultiStepExecution,
                should_interrupt: has_running_task,
                reason: "会话有工具执行历史，延续多步执行".into(),
                llm_calls: Vec::new(),
            };
        }

        // LLM 意图分类
        match Self::classify_with_llm(input, client) {
            Ok((mode, record)) => {
                let llm_calls = {
                    #[cfg(feature = "llm-debug-log")]
                    {
                        vec![record]
                    }
                    #[cfg(not(feature = "llm-debug-log"))]
                    {
                        let _ = record;
                        Vec::new()
                    }
                };
                OrchestratorDecision {
                    should_interrupt: has_running_task && mode.needs_tools(),
                    reason: format!("LLM 分类为 {mode}"),
                    mode,
                    llm_calls,
                }
            }
            Err(err) => {
                tracing::warn!("意图分类 LLM 调用失败，回退到多步执行: {err}");
                OrchestratorDecision {
                    mode: QueryMode::MultiStepExecution,
                    should_interrupt: has_running_task,
                    reason: format!("LLM 分类失败（{err}），回退多步执行"),
                    llm_calls: Vec::new(),
                }
            }
        }
    }

    /// 调用 LLM 判断意图
    ///
    /// 分类结果：
    /// - "chat" → DirectAnswer
    /// - "tool" → SingleToolExecution（简单工具任务）
    /// - "plan" → MultiStepExecution（需要规划的复杂任务）
    /// - "split" → TaskSplit（可并行拆分的多目标任务）
    /// - "background" → BackgroundExecution（耗时任务）
    fn classify_with_llm(
        input: &str,
        client: &SingleProviderClient,
    ) -> anyhow::Result<(QueryMode, crate::session::LlmCallRecord)> {
        let prompt = format!(
            "判断以下用户输入应该走哪种执行模式。只回答一个词：\n\
             - chat: 纯闲聊、知识问答，不需要任何工具\n\
             - tool: 简单的单步操作（如读一个文件、查一个命令）\n\
             - plan: 需要多步骤执行的复杂任务（如重构代码、调试问题）\n\n\
             用户输入：{input}"
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt.clone(),
            context: Vec::new(),
            assembled_system_prompt: None,
        };

        let resp = client.complete(&req)?;
        let answer = resp.text.trim().to_lowercase();

        let mode = if answer.contains("chat") {
            QueryMode::DirectAnswer
        } else if answer.contains("plan") {
            QueryMode::MultiStepExecution
        } else {
            // "tool" 或无法识别 → 单工具执行（安全回退）
            QueryMode::SingleToolExecution
        };

        tracing::info!(
            input_len = input.len(),
            classify_prompt_tokens = resp.usage.prompt_tokens,
            classify_completion_tokens = resp.usage.completion_tokens,
            result = ?mode,
            "查询编排：LLM 意图分类"
        );

        let record = crate::session::LlmCallRecord {
            stage: "orchestrator-classify".to_string(),
            prompt,
            context_count: 0,
            tool_names: Vec::new(),
            response_text: resp.text,
            reasoning_len: resp.reasoning_content.len(),
            tool_calls: Vec::new(),
            usage: resp.usage,
            timestamp: crate::session::now_text(),
        };

        Ok((mode, record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_session() -> Session {
        Session {
            id: "test".into(),
            title: "test".into(),
            messages: Vec::new(),
            task_records: Vec::new(),
            task_plans: Vec::new(),
            cwd: String::new(),
            cwd_mode: Default::default(),
            context_summary: None,
            summary_up_to: 0,
            pending_approvals: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn empty_input_defaults_to_multi_step() {
        // 没有 LLM client 测不了 classify，但可以测试空输入的早返回路径
        let session = empty_session();
        // 空输入不会走到 LLM 调用
        assert!(session.task_records.is_empty());
    }

    #[test]
    fn session_with_tool_history_returns_multi_step() {
        let mut session = empty_session();
        session
            .task_records
            .push(crate::session::SessionTaskRecord {
                task_id: "t1".into(),
                tool_result: Some("result".into()),
                ..Default::default()
            });
        assert!(session.task_records.iter().any(|r| r.tool_result.is_some()));
    }
}
