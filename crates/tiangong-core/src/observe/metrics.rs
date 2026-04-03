//! 指标采集
//!
//! 记录会话/模型/工具的延迟、错误率等运行时指标。

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// 单次操作的计时记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationMetric {
    /// 操作名称
    pub name: String,
    /// 操作类型（llm_call / tool_execution / context_assembly 等）
    pub operation_type: String,
    /// 耗时（毫秒）
    pub duration_ms: u64,
    /// 是否成功
    pub success: bool,
    /// 时间戳
    pub timestamp: String,
}

/// 聚合指标
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregatedMetrics {
    /// LLM 调用次数
    pub llm_call_count: usize,
    /// LLM 调用总耗时（毫秒）
    pub llm_total_duration_ms: u64,
    /// LLM 平均耗时（毫秒）
    pub llm_avg_duration_ms: u64,
    /// 工具执行次数
    pub tool_execution_count: usize,
    /// 工具执行成功次数
    pub tool_success_count: usize,
    /// 工具执行总耗时（毫秒）
    pub tool_total_duration_ms: u64,
    /// 各工具调用次数
    pub tool_call_counts: HashMap<String, usize>,
    /// 错误次数
    pub error_count: usize,
}

/// 指标收集器
#[derive(Debug, Default)]
pub struct MetricsCollector {
    metrics: Vec<OperationMetric>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次操作
    pub fn record(&mut self, metric: OperationMetric) {
        self.metrics.push(metric);
    }

    /// 记录 LLM 调用
    pub fn record_llm_call(&mut self, model: &str, duration: std::time::Duration, success: bool) {
        self.record(OperationMetric {
            name: model.to_string(),
            operation_type: "llm_call".to_string(),
            duration_ms: duration.as_millis() as u64,
            success,
            timestamp: chrono::Local::now().naive_local().to_string(),
        });
    }

    /// 记录工具执行
    pub fn record_tool_execution(&mut self, tool_name: &str, duration_ms: u64, success: bool) {
        self.record(OperationMetric {
            name: tool_name.to_string(),
            operation_type: "tool_execution".to_string(),
            duration_ms,
            success,
            timestamp: chrono::Local::now().naive_local().to_string(),
        });
    }

    /// 计算聚合指标
    pub fn aggregate(&self) -> AggregatedMetrics {
        let mut result = AggregatedMetrics::default();

        for m in &self.metrics {
            match m.operation_type.as_str() {
                "llm_call" => {
                    result.llm_call_count += 1;
                    result.llm_total_duration_ms += m.duration_ms;
                }
                "tool_execution" => {
                    result.tool_execution_count += 1;
                    if m.success {
                        result.tool_success_count += 1;
                    }
                    result.tool_total_duration_ms += m.duration_ms;
                    *result.tool_call_counts.entry(m.name.clone()).or_insert(0) += 1;
                }
                _ => {}
            }
            if !m.success {
                result.error_count += 1;
            }
        }

        if result.llm_call_count > 0 {
            result.llm_avg_duration_ms = result.llm_total_duration_ms / result.llm_call_count as u64;
        }

        result
    }

    /// 清空指标
    pub fn clear(&mut self) {
        self.metrics.clear();
    }
}

/// 计时器辅助
pub struct Timer {
    start: Instant,
}

impl Timer {
    pub fn start() -> Self {
        Self { start: Instant::now() }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_empty() {
        let collector = MetricsCollector::new();
        let agg = collector.aggregate();
        assert_eq!(agg.llm_call_count, 0);
        assert_eq!(agg.tool_execution_count, 0);
    }

    #[test]
    fn aggregate_mixed() {
        let mut collector = MetricsCollector::new();
        collector.record_llm_call("gpt-4", std::time::Duration::from_millis(500), true);
        collector.record_llm_call("gpt-4", std::time::Duration::from_millis(300), true);
        collector.record_tool_execution("read_file", 10, true);
        collector.record_tool_execution("run_command", 5000, false);

        let agg = collector.aggregate();
        assert_eq!(agg.llm_call_count, 2);
        assert_eq!(agg.llm_avg_duration_ms, 400);
        assert_eq!(agg.tool_execution_count, 2);
        assert_eq!(agg.tool_success_count, 1);
        assert_eq!(agg.error_count, 1);
        assert_eq!(agg.tool_call_counts.get("read_file"), Some(&1));
    }
}
