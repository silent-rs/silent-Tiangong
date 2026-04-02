//! TaskCoordinator：任务拆分、分配、汇总
//!
//! 判断复杂任务是否需要拆分为多个 Worker 并行执行，
//! 汇总各 Worker 结果生成最终回复。

use crate::model::TokenUsage;
use crate::runtime::RuntimeEngine;
use crate::session::Session;

use super::types::{
    CoordinatorResult, CoordinatorTask, WorkerBudget, WorkerContext, WorkerResult,
};
use super::worker::Worker;

/// 任务协调器
pub struct TaskCoordinator {
    engine: RuntimeEngine,
}

impl TaskCoordinator {
    pub fn new(engine: RuntimeEngine) -> Self {
        Self { engine }
    }

    /// 判断任务是否需要拆分为多代理
    ///
    /// 当前版本始终返回 false（单 Worker 模式），
    /// 后续版本将基于任务复杂度、子任务独立性等判断。
    pub fn should_split(&self, _task: &CoordinatorTask) -> bool {
        // TODO: 基于以下条件判断：
        // - 任务包含多个可并行的子目标
        // - 子任务上下文差异明显
        // - 需要隔离不同执行环境
        false
    }

    /// 协调执行任务
    pub fn coordinate(&self, task: CoordinatorTask, session: &Session) -> anyhow::Result<CoordinatorResult> {
        if !self.should_split(&task) {
            // 单 Worker 模式（退化为当前行为）
            return self.run_single(task, session);
        }

        // 多 Worker 模式（后续实现）
        let sub_tasks = self.split_task(&task);
        let results = self.run_parallel(sub_tasks, session);
        self.merge_results(results)
    }

    /// 单 Worker 执行（退化模式）
    fn run_single(&self, task: CoordinatorTask, session: &Session) -> anyhow::Result<CoordinatorResult> {
        let worker_context = WorkerContext {
            worker_id: scru128::new().to_string(),
            task_objective: task.user_input.clone(),
            available_tools: Vec::new(), // 空 = 全部工具
            context_scope: super::types::ContextScope::Full,
            working_dir: None,
            budget: WorkerBudget::default(),
        };

        let worker = Worker::new(worker_context, self.engine.clone(), session.clone());
        let result = worker.run();

        let total_usage = result.usage.clone();
        Ok(CoordinatorResult {
            final_response: result.result_text.clone(),
            worker_results: vec![result],
            total_usage,
        })
    }

    /// 拆分任务为子任务（后续实现）
    #[allow(dead_code)]
    fn split_task(&self, _task: &CoordinatorTask) -> Vec<CoordinatorTask> {
        // TODO: 使用 LLM 拆分任务
        vec![]
    }

    /// 并行执行多个子任务（后续实现）
    #[allow(dead_code)]
    fn run_parallel(&self, tasks: Vec<CoordinatorTask>, session: &Session) -> Vec<WorkerResult> {
        // TODO: 使用 thread::scope 并行执行
        tasks
            .into_iter()
            .map(|task| {
                let worker_context = WorkerContext {
                    worker_id: scru128::new().to_string(),
                    task_objective: task.user_input,
                    available_tools: Vec::new(),
                    context_scope: super::types::ContextScope::Full,
                    working_dir: None,
                    budget: WorkerBudget::default(),
                };
                Worker::new(worker_context, self.engine.clone(), session.clone()).run()
            })
            .collect()
    }

    /// 合并多个 Worker 结果（后续实现）
    #[allow(dead_code)]
    fn merge_results(&self, results: Vec<WorkerResult>) -> anyhow::Result<CoordinatorResult> {
        let mut total_usage = TokenUsage::default();
        let mut combined_text = String::new();

        for result in &results {
            total_usage.accumulate(&result.usage);
            if result.success {
                combined_text.push_str(&result.result_text);
                combined_text.push_str("\n\n");
            }
        }

        Ok(CoordinatorResult {
            final_response: combined_text.trim().to_string(),
            worker_results: results,
            total_usage,
        })
    }
}
