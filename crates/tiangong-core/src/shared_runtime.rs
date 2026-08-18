//! 进程级共享 tokio runtime + turn task 管理。
//!
//! 所有 TiangongCore 的 turn task 都跑在这个共享 runtime 上。
//! turn task 在 deliver(Message) 空闲起轮时 spawn，结束（含取消）后自动清理。
//!
//! runtime 必须是 multi-thread：LLM crate 的 `provider_client` 内部使用
//! `tokio::task::block_in_place`（仅在 multi-thread runtime 可用）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::core::command::Command;
use crate::core::plugin::PluginFeedbackTx;
use crate::interaction::{ApprovalChallenges, ApprovalGrants, InteractionRegistry};
use crate::turn_context::TurnContext;

/// 共享 runtime 的 worker 线程数。
const WORKER_THREADS: usize = 2;

static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// turn task 注册表: session_id → 当前代任务。
///
/// cmd_tx 的生命周期与 turn task 绑定。turn task 内部持有 cmd_rx，
/// deliver 侧通过 send_command 投递到活跃 turn task；任务结束移除条目后
/// 迟到命令发送失败（天然反馈封口）。
struct TurnTask {
    generation: u64,
    cmd_tx: UnboundedSender<Command>,
    handle: JoinHandle<()>,
}

type TurnTaskMap = HashMap<String, TurnTask>;

static TURN_TASKS: OnceLock<Mutex<TurnTaskMap>> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn turn_tasks() -> &'static Mutex<TurnTaskMap> {
    TURN_TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程级交互设施：请求注册表、审批授权表与挑战表（交互模型方案 §7/§12/§13）。
/// 授权与挑战为运行期内存态（会话隔离、重启失效）。
pub struct InteractionHub {
    pub registry: InteractionRegistry,
    pub grants: ApprovalGrants,
    pub challenges: ApprovalChallenges,
}

static INTERACTION_HUB: OnceLock<InteractionHub> = OnceLock::new();

/// 进程级交互设施单例。
///
/// 初始化即挂闭合通知链：请求闭合（响应/超时/取消，唯一胜者）后唤醒等待中的
/// request_user 工具 Future——声明式插件经标准工具流水线等待，无需感知命令通道。
pub fn interactions() -> &'static InteractionHub {
    INTERACTION_HUB.get_or_init(|| {
        let hub = InteractionHub {
            registry: InteractionRegistry::new(),
            grants: ApprovalGrants::new(),
            challenges: ApprovalChallenges::new(),
        };
        hub.registry.set_close_handler(std::sync::Arc::new(
            |closed: crate::interaction::ClosedInteraction| {
                if let Some(sender) = crate::interaction::request_waiters_private()
                    .lock()
                    .ok()
                    .and_then(|mut waiters| waiters.remove(&closed.request.request_id))
                {
                    // 接收端已 drop（工具 Future 被中断）时忽略，请求由取消/超时路径闭合
                    let _ = sender.send(Box::new(closed));
                }
            },
        ));
        hub
    })
}

/// 获取进程级共享 tokio runtime。
pub fn shared_runtime() -> &'static Runtime {
    SHARED_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("创建共享 tokio runtime 失败")
    })
}

/// 为已经构建完成的 [`TurnContext`] 创建命令通道并 Spawn turn task。
///
/// 本函数先把本轮 feedback 通道注入 Context 中的插件，再调用 `future_factory`
/// 同步完成任务启动前的准备并生成 Future。只有上述步骤全部成功后才注册并启动任务，
/// 避免留下半初始化运行态。
///
/// turn task 正常结束时，wrapper 会按任务代际立即清理；panic 的任务保留在表中，
/// 供关闭路径等待并上报。
pub fn spawn_turn<F, Fut>(
    context: TurnContext,
    future_factory: F,
) -> Result<(), crate::core::CoreError>
where
    F: FnOnce(TurnContext, UnboundedReceiver<Command>) -> Result<Fut, crate::core::CoreError>,
    Fut: Future<Output = ()> + Send + 'static,
{
    let session_id = context.session.id.clone();
    if is_running(&session_id) {
        return Err(crate::core::CoreError::Busy);
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    for plugin in &context.plugins {
        plugin.set_feedback_tx(PluginFeedbackTx::new(cmd_tx.clone()));
    }
    let future = future_factory(context, cmd_rx)?;

    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let (start_tx, start_rx) = oneshot::channel();
    let sid = session_id.clone();
    let handle = shared_runtime().spawn(async move {
        if start_rx.await.is_ok() {
            future.await;
        }
        remove_turn_if_current(&sid, generation);
    });
    let mut tasks = turn_tasks()
        .lock()
        .map_err(|_| crate::core::CoreError::WorkerStopped)?;
    if let Some(task) = tasks.get(&session_id) {
        handle.abort();
        return Err(if task.handle.is_finished() {
            crate::core::CoreError::WorkerPanicked
        } else {
            crate::core::CoreError::Busy
        });
    }
    tasks.insert(
        session_id,
        TurnTask {
            generation,
            cmd_tx,
            handle,
        },
    );
    drop(tasks);
    start_tx
        .send(())
        .map_err(|_| crate::core::CoreError::WorkerStopped)
}

/// 向当前活跃任务发送命令（取消、审批、工具注入和运行配置更新等）。
///
/// 无活跃任务时返回 false(命令被忽略)。
pub fn send_command(session_id: &str, cmd: Command) -> bool {
    if let Ok(tasks) = turn_tasks().lock()
        && let Some(task) = tasks.get(session_id)
        && !task.handle.is_finished()
    {
        return task.cmd_tx.send(cmd).is_ok();
    }
    false
}

fn remove_turn_if_current(session_id: &str, generation: u64) {
    if let Ok(mut tasks) = turn_tasks().lock()
        && tasks
            .get(session_id)
            .is_some_and(|task| task.generation == generation)
    {
        tasks.remove(session_id);
    }
}

/// 立即注销指定会话的活跃任务条目。
///
/// 供任务 wrapper 在结束前需要后继动作（如手动压缩被用户消息打断后起新轮）时
/// 腾出注册表槽位使用；普通结束路径无需调用（wrapper 自动按代际清理）。
pub fn release_agent(session_id: &str) {
    if let Ok(mut tasks) = turn_tasks().lock() {
        tasks.remove(session_id);
    }
}

/// 取消并等待指定 session 的活跃 turn 完成。
pub fn cancel_and_join(session_id: &str) -> Result<(), crate::core::CoreError> {
    let handle = {
        let mut tasks = turn_tasks()
            .lock()
            .map_err(|_| crate::core::CoreError::WorkerStopped)?;
        let Some(task) = tasks.remove(session_id) else {
            return Ok(());
        };
        let _ = task.cmd_tx.send(Command::Cancel);
        task.handle
    };

    std::thread::scope(|scope| {
        scope
            .spawn(move || shared_runtime().block_on(handle))
            .join()
            .map_err(|_| crate::core::CoreError::WorkerPanicked)?
            .map_err(|_| crate::core::CoreError::WorkerPanicked)
    })
}

/// 查询指定 session 是否有活跃的 turn task。
pub fn is_running(session_id: &str) -> bool {
    if let Ok(tasks) = turn_tasks().lock() {
        tasks
            .get(session_id)
            .is_some_and(|task| !task.handle.is_finished())
    } else {
        false
    }
}
