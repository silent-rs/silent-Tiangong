//! TaskCoordinator：任务拆分、分配、汇总
//!
//! 使用 LLM 判断是否需要多代理并行执行，
//! 拆分子任务后分配给独立 Worker 并行执行，
//! 汇总结果生成最终回复。

use std::sync::mpsc::{self, Receiver};

use crate::app_state::{ControlSignal, TurnEvent};
use crate::model::{ModelClient, ModelRequest, TokenUsage};
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

    /// 使用 LLM 判断任务是否需要拆分为多代理
    pub fn should_split(&self, task: &CoordinatorTask) -> bool {
        let prompt = format!(
            "判断以下任务是否可以拆分为多个独立的子任务并行执行。\n\
             只回答 yes 或 no。\n\
             拆分条件：任务包含多个明确的独立子目标，且子目标之间没有依赖关系。\n\n\
             任务：{}", task.user_input
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt,
            context: Vec::new(),
        };

        match self.engine.client().complete(&req) {
            Ok(resp) => {
                let answer = resp.text.trim().to_lowercase();
                let should = answer.contains("yes");
                tracing::info!(
                    task_input_len = task.user_input.len(),
                    should_split = should,
                    "多代理拆分判断"
                );
                should
            }
            Err(err) => {
                tracing::warn!("多代理拆分判断失败，使用单 Worker: {err}");
                false
            }
        }
    }

    /// 协调执行任务
    pub fn coordinate(
        &self,
        task: CoordinatorTask,
        session: &Session,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
        ctrl_rx: Option<Receiver<ControlSignal>>,
    ) -> anyhow::Result<CoordinatorResult> {
        if !self.should_split(&task) {
            return self.run_single(task, session, event_tx, ctrl_rx);
        }

        let sub_tasks = self.split_task(&task)?;
        if sub_tasks.len() <= 1 {
            let single_task = sub_tasks.into_iter().next().unwrap_or(task);
            return self.run_single(single_task, session, event_tx, ctrl_rx);
        }

        tracing::info!(sub_task_count = sub_tasks.len(), "多 Worker 并行执行");
        let results = self.run_parallel(sub_tasks, session, event_tx);
        self.merge_results(&task, results)
    }

    /// 单 Worker 执行（退化模式，透传控制信号）
    fn run_single(
        &self,
        task: CoordinatorTask,
        session: &Session,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
        ctrl_rx: Option<Receiver<ControlSignal>>,
    ) -> anyhow::Result<CoordinatorResult> {
        let worker_context = WorkerContext {
            worker_id: scru128::new().to_string(),
            task_objective: task.user_input.clone(),
            available_tools: Vec::new(),
            context_scope: super::types::ContextScope::Full,
            working_dir: None,
            budget: WorkerBudget::default(),
        };

        let mut worker = Worker::new(worker_context, self.engine.clone(), session.clone());
        if let Some(tx) = event_tx {
            worker = worker.with_event_tx(tx.clone());
        }
        if let Some(rx) = ctrl_rx {
            worker = worker.with_ctrl_rx(rx);
        }
        let result = worker.run();

        let total_usage = result.usage.clone();
        Ok(CoordinatorResult {
            final_response: result.result_text.clone(),
            worker_results: vec![result],
            total_usage,
        })
    }

    /// 使用 LLM 拆分任务为子任务
    fn split_task(&self, task: &CoordinatorTask) -> anyhow::Result<Vec<CoordinatorTask>> {
        let prompt = format!(
            "将以下任务拆分为可独立并行执行的子任务。\n\
             每个子任务用一行描述，格式：`- 子任务描述`\n\
             只列出子任务，不要额外说明。\n\n\
             任务：{}", task.user_input
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt,
            context: Vec::new(),
        };

        let resp = self.engine.client().complete(&req)?;

        let sub_tasks: Vec<CoordinatorTask> = resp.text
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim().trim_start_matches('-').trim();
                if trimmed.is_empty() { return None; }
                Some(CoordinatorTask {
                    id: scru128::new().to_string(),
                    objective: trimmed.to_string(),
                    user_input: trimmed.to_string(),
                    context: task.context.clone(),
                })
            })
            .collect();

        tracing::info!(
            original = task.user_input.chars().take(50).collect::<String>(),
            sub_tasks = sub_tasks.len(),
            "任务拆分完成"
        );

        Ok(sub_tasks)
    }

    /// 并行执行多个子任务
    fn run_parallel(
        &self,
        tasks: Vec<CoordinatorTask>,
        session: &Session,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Vec<WorkerResult> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = tasks
                .into_iter()
                .enumerate()
                .map(|(i, task)| {
                    let engine = self.engine.clone();
                    let session = session.clone();
                    let worker_id = format!("worker-{}-{}", i, scru128::new());
                    let tx = event_tx.cloned();

                    scope.spawn(move || {
                        let worker_context = WorkerContext {
                            worker_id: worker_id.clone(),
                            task_objective: task.user_input,
                            available_tools: Vec::new(),
                            context_scope: super::types::ContextScope::Full,
                            working_dir: None,
                            budget: WorkerBudget::default(),
                        };

                        tracing::info!(worker_id, "Worker 开始执行");
                        let mut worker = Worker::new(worker_context, engine, session);
                        if let Some(tx) = tx {
                            worker = worker.with_event_tx(tx);
                        }
                        let result = worker.run();
                        tracing::info!(
                            worker_id,
                            success = result.success,
                            duration_ms = result.duration_ms,
                            "Worker 执行完成"
                        );
                        result
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| WorkerResult {
                        worker_id: "unknown".to_string(),
                        result_text: String::new(),
                        success: false,
                        error: Some("Worker 线程 panic".to_string()),
                        usage: Default::default(),
                        duration_ms: 0,
                    })
                })
                .collect()
        })
    }

    /// 使用 LLM 合并多个 Worker 结果
    fn merge_results(
        &self,
        original_task: &CoordinatorTask,
        results: Vec<WorkerResult>,
    ) -> anyhow::Result<CoordinatorResult> {
        let mut total_usage = TokenUsage::default();
        let mut worker_outputs = String::new();
        for (i, result) in results.iter().enumerate() {
            total_usage.accumulate(&result.usage);
            if result.success {
                worker_outputs.push_str(&format!(
                    "### Worker {} 结果\n{}\n\n",
                    i + 1,
                    result.result_text
                ));
            } else {
                worker_outputs.push_str(&format!(
                    "### Worker {} 失败\n错误：{}\n\n",
                    i + 1,
                    result.error.as_deref().unwrap_or("未知错误")
                ));
            }
        }

        // 使用 LLM 合成最终回复
        let final_response = if results.len() == 1 {
            results[0].result_text.clone()
        } else {
            let prompt = format!(
                "以下是多个 Worker 并行执行的结果，请合成一个完整的最终回复。\n\
                 原始任务：{}\n\n{}", original_task.user_input, worker_outputs
            );

            let req = ModelRequest {
                session_title: String::new(),
                user_input: prompt,
                context: Vec::new(),
            };

            match self.engine.client().complete(&req) {
                Ok(resp) => {
                    total_usage.accumulate(&resp.usage);
                    resp.text
                }
                Err(_) => {
                    // LLM 合成失败，直接拼接
                    worker_outputs
                }
            }
        };

        Ok(CoordinatorResult {
            final_response,
            worker_results: results,
            total_usage,
        })
    }
}
