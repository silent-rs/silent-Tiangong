//! 观测与成本层
//!
//! 提供运行时指标采集、Token 成本追踪、审计记录等观测能力。

pub mod audit;
pub mod collector;
pub mod cost;
pub mod metrics;

pub use audit::{
    AuditEventType, AuditRecord, audit_permission, audit_permission_with_context,
    audit_tool_execution, audit_trust_mode_changed,
};
pub use collector::ObserveCollector;
pub use cost::{
    CostSummary, RequestCost, SessionCost, TaskCost, build_session_cost, calculate_session_cost,
};
pub use metrics::{AggregatedMetrics, MetricsCollector, OperationMetric, Timer};
