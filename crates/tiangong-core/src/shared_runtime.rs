//! 进程级共享 tokio runtime + turn task 管理。
//!
//! 所有 TiangongCore 的 turn task 都跑在这个共享 runtime 上。
//! turn task 在 deliver(Message) 时 spawn,turn 结束后自动清理。
//!
//! runtime 必须是 multi-thread：LLM crate 的 `provider_client` 内部使用
//! `tokio::task::block_in_place`（仅在 multi-thread runtime 可用）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::core::command::Command;

/// 共享 runtime 的 worker 线程数。
const WORKER_THREADS: usize = 2;

/// GC 扫描间隔(清理 panic 的 turn task)。
const GC_INTERVAL: Duration = Duration::from_millis(500);

static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// turn task 注册表:session_id → (cmd_tx, JoinHandle)
///
/// cmd_tx 的生命周期与 turn task 绑定。turn task 内部持有 cmd_rx,
/// deliver(Cancel/Approval) 通过 send_command 投递到活跃 turn task。
type TurnTaskMap = HashMap<String, (UnboundedSender<Command>, JoinHandle<()>)>;

static TURN_TASKS: OnceLock<Mutex<TurnTaskMap>> = OnceLock::new();

fn turn_tasks() -> &'static Mutex<TurnTaskMap> {
    TURN_TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 获取进程级共享 tokio runtime。
pub fn shared_runtime() -> &'static Runtime {
    SHARED_RUNTIME.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("创建共享 tokio runtime 失败");

        // 启动 GC task:定期清理已结束(含 panic)的 turn task。
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(GC_INTERVAL).await;
                if let Ok(mut tasks) = turn_tasks().lock() {
                    tasks.retain(|_id, (_, handle)| !handle.is_finished());
                }
            }
        });

        runtime
    })
}

/// Spawn 一个 turn task,创建专属的命令通道并注册到 turn_tasks。
///
/// `fut_fn` 接收 cmd_rx(用于在 turn 内消费 Cancel/Approval/InjectTool)。
///
/// turn task 正常结束或被 abort 后,wrapper 会立即调 `remove_turn` 清理。
/// panic 的 task 由 GC task 定期清理。
pub fn spawn_turn<F, Fut>(session_id: String, fut_fn: F)
where
    F: FnOnce(UnboundedReceiver<Command>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let sid = session_id.clone();
    let handle = shared_runtime().spawn(async move {
        fut_fn(cmd_rx).await;
        // 正常结束或 abort → 立即清理
        remove_turn(&sid);
    });
    if let Ok(mut tasks) = turn_tasks().lock() {
        tasks.insert(session_id, (cmd_tx, handle));
    }
}

/// 向活跃 turn task 发送命令(Cancel/Approval/InjectTool)。
///
/// 无活跃 turn task 时返回 false(命令被忽略)。
pub fn send_command(session_id: &str, cmd: Command) -> bool {
    if let Ok(tasks) = turn_tasks().lock() {
        if let Some((tx, handle)) = tasks.get(session_id) {
            if !handle.is_finished() {
                return tx.send(cmd).is_ok();
            }
        }
    }
    false
}

/// 从 turn_tasks 移除指定 session。
pub fn remove_turn(session_id: &str) {
    if let Ok(mut tasks) = turn_tasks().lock() {
        tasks.remove(session_id);
    }
}

/// 查询指定 session 是否有活跃的 turn task。
pub fn is_running(session_id: &str) -> bool {
    if let Ok(tasks) = turn_tasks().lock() {
        tasks
            .get(session_id)
            .is_some_and(|(_, handle)| !handle.is_finished())
    } else {
        false
    }
}
