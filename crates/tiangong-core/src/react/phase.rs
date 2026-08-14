//! ExecutionPhase 驱动原型（任务 02）。
//!
//! 验证"取出阶段 → 等待一个事件 → 安装新阶段"的所有权模式：阶段持有 JoinHandle/
//! JoinSet 时可安全 take/drive/install，无需在 state 上维护并列活动 `Option`。
//! 任务 03 在此骨架上扩展为正式 `ExecutionPhase` / `ExecutionState`。

// 原型仅用于验证所有权模式，尚未接入生产 execute_turn；任务 03 转为正式类型后
// 移除该 allow。
#![allow(dead_code)]

use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::command::Command;

/// 最小阶段原型：覆盖 Ready（NeedModel/PendingFinish）与 Waiting（持有 JoinHandle/
/// JoinSet）两类，验证所有权迁移。
pub(super) enum ProtoPhase {
    NeedModel,
    WaitingModel(tokio::task::JoinHandle<&'static str>),
    WaitingTools(tokio::task::JoinSet<&'static str>),
    PendingFinish,
}

enum ProtoEffect {
    Continue,
    Done,
}

/// 验证用状态：phase 在 take/install 之间为 None，断言约束"无双阶段"。
pub(super) struct ProtoState {
    phase: Option<ProtoPhase>,
}

impl ProtoState {
    pub(super) fn new() -> Self {
        Self {
            phase: Some(ProtoPhase::NeedModel),
        }
    }

    /// 取出当前阶段。取出后到 install 之间为"无阶段"，调用方必须保证所有退出路径
    /// 都 install 新阶段或返回最终结果（ALR-205 无半迁移状态）。
    pub(super) fn take_phase(&mut self) -> ProtoPhase {
        self.phase.take().expect("take_phase 时阶段必须存在")
    }

    /// 安装新阶段。install 前 phase 必须为 None（已 take），避免双阶段并存。
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

/// 驱动一个阶段：消费 phase，返回新阶段与效果。
///
/// 这是后续 `ExecutionDriver` 的所有权骨架：phase 独立持有活动资源（JoinHandle/
/// JoinSet），迁移时整体转移，无需在 state 上维护并列 `Option`。
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
            // Waiting 阶段：持有 JoinHandle，等待命令或任务完成。
            // 取消用 AbortHandle，避免与 select 消费 handle 冲突。
            let abort = handle.abort_handle();
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => match cmd {
                    Some(_) => {
                        abort.abort();
                        (ProtoPhase::NeedModel, ProtoEffect::Continue)
                    }
                    None => (ProtoPhase::PendingFinish, ProtoEffect::Done),
                },
                _ = handle => (ProtoPhase::PendingFinish, ProtoEffect::Continue),
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

/// 单事件循环骨架：take → drive → install，直到 Done。
///
/// 证明 take/install 模式在循环中可行，且 drive 持有阶段资源时无需并列 Option。
pub(super) async fn proto_drive_loop(
    state: &mut ProtoState,
    cmd_rx: &mut UnboundedReceiver<Command>,
) {
    loop {
        let phase = state.take_phase();
        let (next, effect) = drive_phase(phase, cmd_rx).await;
        state.install_phase(next);
        if matches!(effect, ProtoEffect::Done) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

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
    async fn command_aborts_model_and_returns_to_need_model() {
        // WaitingModel 期间命令到达：abort 旧任务，回到 NeedModel。
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        let mut state = ProtoState::new();
        // 推进到 WaitingModel。
        let phase = state.take_phase();
        let (next, _) = drive_phase(phase, &mut rx).await;
        assert!(matches!(next, ProtoPhase::WaitingModel(_)));
        state.install_phase(next);
        // 投一条命令，WaitingModel 收到 → abort → NeedModel。
        tx.send(Command::Cancel).unwrap();
        let phase = state.take_phase();
        let (next, _) = drive_phase(phase, &mut rx).await;
        assert!(matches!(next, ProtoPhase::NeedModel));
        state.install_phase(next);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn joinset_holding_phase_can_migrate() {
        // WaitingTools 持有 JoinSet，任务完成后迁移到 PendingFinish。
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async { "tool-output" });
        let (_tx, mut rx) = mpsc::unbounded_channel::<Command>();
        let (next, _) = drive_phase(ProtoPhase::WaitingTools(tasks), &mut rx).await;
        assert!(matches!(next, ProtoPhase::PendingFinish));
    }
}
