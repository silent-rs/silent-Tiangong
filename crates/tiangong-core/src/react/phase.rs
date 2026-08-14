//! ExecutionPhase 驱动原型（任务 02）。
//!
//! 验证"取出阶段 → 等待事件 → 安装新阶段"的所有权模式，重点覆盖：
//! - 阶段持有 JoinHandle/JoinSet 时可安全迁移，无需并列活动 `Option`；
//! - 运行中的任务被命令取消时，abort 后**等待其真正结束**，不残留迟到结果；
//! - 迁移中断（panic/future 取消）时由守卫恢复明确阶段，不留下"无阶段"半状态。
//!
//! 任务 03 在此骨架上扩展为正式 `ExecutionPhase` / `ExecutionState`。

// 原型仅用于验证所有权与取消模式，尚未接入生产 execute_turn；任务 03 转为正式
// 类型后移除该 allow。
#![allow(dead_code)]

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{JoinHandle, JoinSet};

use crate::core::command::Command;

/// 最小阶段原型：覆盖 Ready（NeedModel/PendingFinish）与 Waiting（持有 JoinHandle/
/// JoinSet）两类。
pub(super) enum ProtoPhase {
    NeedModel,
    WaitingModel(JoinHandle<&'static str>),
    WaitingTools(JoinSet<&'static str>),
    PendingFinish,
}

enum ProtoEffect {
    Continue,
    Done,
}

/// `WaitingModel` 等待的结果：收到命令、任务完成、或命令通道关闭。
enum WaitOutcome {
    Command,
    Completed,
    Closed,
}

pub(super) struct ProtoState {
    phase: Option<ProtoPhase>,
}

impl ProtoState {
    pub(super) fn new() -> Self {
        Self {
            phase: Some(ProtoPhase::NeedModel),
        }
    }

    /// 取出当前阶段。取出后到 install 之间为"无阶段"，由 [`InstallGuard`] 保证所有
    /// 退出路径都 install 新阶段或恢复安全阶段（ALR-205）。
    pub(super) fn take_phase(&mut self) -> ProtoPhase {
        self.phase.take().expect("take_phase 时阶段必须存在")
    }

    pub(super) fn install_phase(&mut self, phase: ProtoPhase) {
        debug_assert!(
            self.phase.is_none(),
            "install_phase 前必须先 take，避免双阶段并存"
        );
        self.phase = Some(phase);
    }
}

impl Default for ProtoState {
    fn default() -> Self {
        Self::new()
    }
}

/// 迁移守卫：take 之后无论正常 install、panic 还是 future 被取消，都保证 state
/// 不会停留在"无阶段"。未 install 时 Drop 恢复为 `PendingFinish`，形成明确终态。
struct InstallGuard<'a> {
    state: &'a mut ProtoState,
    installed: bool,
}

impl InstallGuard<'_> {
    fn install(&mut self, phase: ProtoPhase) {
        self.state.install_phase(phase);
        self.installed = true;
    }
}

impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        if !self.installed {
            tracing::error!(
                "阶段迁移中断（panic 或 future 取消）：未安装新阶段，恢复为 PendingFinish"
            );
            // 中断时 phase 已 take（确为 None），直接恢复，绕过 install_phase 的断言。
            self.state.phase = Some(ProtoPhase::PendingFinish);
        }
    }
}

/// 驱动一个阶段：消费 phase，返回新阶段与效果。
async fn drive_phase(
    phase: ProtoPhase,
    cmd_rx: &mut UnboundedReceiver<Command>,
) -> (ProtoPhase, ProtoEffect) {
    match phase {
        ProtoPhase::NeedModel => {
            // Ready 阶段：同步推进，启动一个"模型任务"后进入 Waiting。
            let handle = tokio::spawn(async { "model-output" });
            (ProtoPhase::WaitingModel(handle), ProtoEffect::Continue)
        }
        ProtoPhase::WaitingModel(handle) => {
            // 命令取消用独立的 AbortHandle，避免与 select 监听 handle 冲突。
            let abort = handle.abort_handle();
            let mut handle = handle;
            let outcome = tokio::select! {
                biased;
                cmd = cmd_rx.recv() => match cmd {
                    Some(_) => WaitOutcome::Command,
                    None => WaitOutcome::Closed,
                },
                _ = &mut handle => WaitOutcome::Completed,
            };
            match outcome {
                WaitOutcome::Command => {
                    abort.abort();
                    // 等待旧任务真正结束，避免残留迟到结果（ALR-204）。
                    let _ = handle.await;
                    (ProtoPhase::NeedModel, ProtoEffect::Continue)
                }
                WaitOutcome::Completed => (ProtoPhase::PendingFinish, ProtoEffect::Continue),
                WaitOutcome::Closed => (ProtoPhase::PendingFinish, ProtoEffect::Done),
            }
        }
        ProtoPhase::WaitingTools(mut tasks) => {
            // Waiting 阶段：持有 JoinSet，等待任务完成（验证 JoinSet 可迁移）。
            if tasks.join_next().await.is_some() {
                (ProtoPhase::PendingFinish, ProtoEffect::Continue)
            } else {
                (ProtoPhase::NeedModel, ProtoEffect::Continue)
            }
        }
        ProtoPhase::PendingFinish => (ProtoPhase::PendingFinish, ProtoEffect::Done),
    }
}

/// 单事件循环骨架：take →（守卫）→ drive → install，直到 Done。
///
/// `InstallGuard` 保证：即便 `drive_phase` panic 或整个 future 被取消，state 也会
/// 恢复为 `PendingFinish`，不会停留在"无阶段"半状态。
pub(super) async fn proto_drive_loop(
    state: &mut ProtoState,
    cmd_rx: &mut UnboundedReceiver<Command>,
) {
    loop {
        let phase = state.take_phase();
        let mut guard = InstallGuard {
            state,
            installed: false,
        };
        let (next, effect) = drive_phase(phase, cmd_rx).await;
        guard.install(next);
        if matches!(effect, ProtoEffect::Done) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::{Notify, mpsc};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn take_drive_install_cycle_completes_without_command() {
        // NeedModel → WaitingModel →（通道关闭）PendingFinish(Done)。
        let mut state = ProtoState::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        drop(tx);
        proto_drive_loop(&mut state, &mut rx).await;
        assert!(matches!(state.phase, Some(ProtoPhase::PendingFinish)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_aborts_running_model_and_waits_for_completion() {
        // 模型任务先确认进入运行（Notify），再发命令；drive 必须 abort 并等待任务
        // 真正结束，且不应等满任务的长期 sleep——以此证明取消真的生效。
        let started = Arc::new(Notify::new());
        let entered = Arc::new(AtomicBool::new(false));
        let task_started = started.clone();
        let task_entered = entered.clone();
        let handle = tokio::spawn(async move {
            task_entered.store(true, Ordering::SeqCst);
            task_started.notify_one();
            // 长期运行，仅 abort 能终止。
            tokio::time::sleep(Duration::from_secs(30)).await;
            "done"
        });
        started.notified().await;
        assert!(entered.load(Ordering::SeqCst), "模型任务应已进入运行");

        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        tx.send(Command::Cancel).unwrap();
        let began = std::time::Instant::now();
        let (next, _) = drive_phase(ProtoPhase::WaitingModel(handle), &mut rx).await;
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "abort 后应快速结束（等待任务取消），实际耗时 {:?}",
            began.elapsed()
        );
        assert!(
            matches!(next, ProtoPhase::NeedModel),
            "命令应使阶段回到 NeedModel"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn joinset_holding_phase_can_migrate() {
        // WaitingTools 持有 JoinSet，任务完成后迁移到 PendingFinish。
        let mut tasks = JoinSet::new();
        tasks.spawn(async { "tool-output" });
        let (_tx, mut rx) = mpsc::unbounded_channel::<Command>();
        let (next, _) = drive_phase(ProtoPhase::WaitingTools(tasks), &mut rx).await;
        assert!(matches!(next, ProtoPhase::PendingFinish));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_guard_restores_pending_finish_on_interrupt() {
        // 模拟迁移中断：take 后守卫未 install 就 drop（如 panic/future 取消）。
        let mut state = ProtoState::new();
        let _phase = state.take_phase();
        assert!(state.phase.is_none(), "take 后应处于无阶段");
        {
            let _guard = InstallGuard {
                state: &mut state,
                installed: false,
            };
            // 不 install，块结束时 guard drop。
        }
        assert!(
            matches!(state.phase, Some(ProtoPhase::PendingFinish)),
            "迁移中断后守卫应恢复为 PendingFinish"
        );
    }
}
