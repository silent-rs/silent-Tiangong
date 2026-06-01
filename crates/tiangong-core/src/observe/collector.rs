//! 统一观测采集入口
//!
//! 聚合 metrics、cost、audit 三个维度的数据采集，
//! 提供单一入口供 TurnRunner 等模块使用。

use crate::model::TokenUsage;

use super::cost::{CostSummary, SessionCost, TaskCost};
use super::metrics::{MetricsCollector, OperationMetric};

/// 统一观测采集器
///
/// 集成指标、成本、审计三个维度的采集能力。
#[derive(Debug, Default)]
pub struct ObserveCollector {
    /// 指标采集器
    metrics: MetricsCollector,
    /// 当前会话成本
    session_cost: Option<SessionCost>,
    /// 当前任务成本
    current_task_cost: Option<TaskCost>,
}

impl ObserveCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// 为会话初始化采集器
    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_cost = Some(SessionCost::new(session_id));
        self
    }

    /// 开始一个新任务的成本追踪
    pub fn begin_task(&mut self, task_id: String) {
        self.current_task_cost = Some(TaskCost::new(task_id));
    }

    /// 记录一次 LLM 调用
    pub fn record_llm_call(&mut self, request_id: String, usage: &TokenUsage) {
        if let Some(ref mut task_cost) = self.current_task_cost {
            task_cost.add_request(request_id, usage);
        }
    }

    /// 记录一次操作指标
    pub fn record_metric(&mut self, metric: OperationMetric) {
        self.metrics.record(metric);
    }

    /// 结束当前任务追踪，将成本归入会话
    pub fn end_task(&mut self) {
        if let Some(task_cost) = self.current_task_cost.take()
            && let Some(ref mut session_cost) = self.session_cost
        {
            session_cost.add_task(task_cost);
        }
    }

    /// 获取当前会话成本摘要
    pub fn session_summary(&self) -> CostSummary {
        self.session_cost
            .as_ref()
            .map(|s| s.summary.clone())
            .unwrap_or_default()
    }

    /// 获取当前任务成本摘要
    pub fn task_summary(&self) -> CostSummary {
        self.current_task_cost
            .as_ref()
            .map(|t| t.summary.clone())
            .unwrap_or_default()
    }

    /// 获取指标采集器引用
    pub fn metrics(&self) -> &MetricsCollector {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_collection_flow() {
        let mut collector = ObserveCollector::new().with_session("s1".into());

        collector.begin_task("t1".into());
        collector.record_llm_call(
            "req1".into(),
            &TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                ..Default::default()
            },
        );
        collector.record_llm_call(
            "req2".into(),
            &TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 100,
                total_tokens: 300,
                ..Default::default()
            },
        );
        collector.end_task();

        let summary = collector.session_summary();
        assert_eq!(summary.total_tokens, 450);
        assert_eq!(summary.call_count, 2);
    }

    #[test]
    fn multiple_tasks() {
        let mut collector = ObserveCollector::new().with_session("s1".into());

        collector.begin_task("t1".into());
        collector.record_llm_call(
            "r1".into(),
            &TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                ..Default::default()
            },
        );
        collector.end_task();

        collector.begin_task("t2".into());
        collector.record_llm_call(
            "r2".into(),
            &TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 100,
                total_tokens: 300,
                ..Default::default()
            },
        );
        collector.end_task();

        let summary = collector.session_summary();
        assert_eq!(summary.total_tokens, 450);
        assert_eq!(summary.call_count, 2);
    }

    #[test]
    fn task_summary_during_execution() {
        let mut collector = ObserveCollector::new().with_session("s1".into());
        collector.begin_task("t1".into());
        collector.record_llm_call(
            "r1".into(),
            &TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                ..Default::default()
            },
        );

        // 任务进行中查询
        let task_summary = collector.task_summary();
        assert_eq!(task_summary.total_tokens, 150);

        // 会话级还没有（任务未结束）
        let session_summary = collector.session_summary();
        assert_eq!(session_summary.total_tokens, 0);
    }
}
