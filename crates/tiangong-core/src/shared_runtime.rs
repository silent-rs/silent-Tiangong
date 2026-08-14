//! 进程级共享 tokio runtime + turn task 管理。
//!
//! 所有 TiangongCore 的 turn task 都跑在这个共享 runtime 上。
//! turn task 在 deliver(Message) 时 spawn,turn 结束后自动清理。
//!
//! runtime 必须是 multi-thread：LLM crate 的 `provider_client` 内部使用
//! `tokio::task::block_in_place`（仅在 multi-thread runtime 可用）。

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::core::command::Command;
use crate::core::plugin::PluginFeedbackTx;
use crate::turn_context::TurnContext;

/// 共享 runtime 的 worker 线程数。
const WORKER_THREADS: usize = 2;

static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// turn 命令入口的接收状态（终态封口，ALR-201；见 design.md 6.2）。
enum IngressState {
    /// 正常接收命令。
    Accepting,
    /// 封口中：已停止接收新命令，正在排空封口前已入队的命令。
    Sealing,
    /// 已确定提交终态：旧队列拒绝新命令，用户消息可靠进入下一 turn 队列。
    Committing,
}

/// 命令入口门控：宿主 `send_command`、插件 `PluginFeedbackTx`、关闭路径共享同一份
/// 接收状态，封口对所有来源一致生效（ALR-201/203）。禁止绕过门控持有裸发送端。
#[derive(Clone)]
pub(crate) struct CommandIngress {
    state: Arc<Mutex<IngressState>>,
    sender: UnboundedSender<Command>,
}

impl CommandIngress {
    pub(crate) fn new(sender: UnboundedSender<Command>) -> Self {
        Self {
            state: Arc::new(Mutex::new(IngressState::Accepting)),
            sender,
        }
    }

    /// `Accepting` 状态下入队并返回 true；`Sealing`/`Committing` 拒绝（返回 false）。
    pub(crate) fn send(&self, cmd: Command) -> bool {
        let state = self.state.lock().unwrap();
        if !matches!(*state, IngressState::Accepting) {
            return false;
        }
        self.sender.send(cmd).is_ok()
    }

    /// 绕过门控强制入队，仅用于 Core 关闭路径的强制取消（此时门控语义已无意义）。
    pub(crate) fn force_send(&self, cmd: Command) -> bool {
        self.sender.send(cmd).is_ok()
    }

    /// `Accepting → Sealing`：此后新命令不再入当前队列。
    pub(crate) fn begin_seal(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, IngressState::Accepting) {
            *state = IngressState::Sealing;
        }
    }

    /// `Sealing → Accepting`：排空发现继续命令（用户消息/工具注入等），恢复接收。
    pub(crate) fn reopen(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, IngressState::Sealing) {
            *state = IngressState::Accepting;
        }
    }

    /// `Sealing → Committing`：确定提交终态。
    pub(crate) fn commit(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, IngressState::Sealing) {
            *state = IngressState::Committing;
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// 当前是否处于 `Accepting`（可接收命令）。`false` 表示正在封口
    /// （`Sealing`/`Committing`）或通道已关闭——插件反馈此时不应投递，
    /// 需要保留待重试数据（ALR-201/203）。
    pub(crate) fn is_accepting(&self) -> bool {
        let state = self.state.lock().unwrap();
        matches!(*state, IngressState::Accepting)
    }
}

/// turn task 注册表: session_id → 当前代任务。
///
/// ingress 的生命周期与 turn task 绑定。turn task 内部持有 cmd_rx,
/// deliver(Cancel/Approval) 通过 send_command 投递到活跃 turn task。
struct TurnTask {
    generation: u64,
    ingress: CommandIngress,
    handle: JoinHandle<()>,
}

type TurnTaskMap = HashMap<String, TurnTask>;

static TURN_TASKS: OnceLock<Mutex<TurnTaskMap>> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn turn_tasks() -> &'static Mutex<TurnTaskMap> {
    TURN_TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 会话级待执行命令队列：终态封口（Sealing/Committing）期间到达的用户消息暂存
/// 于此，由 deliver 的下一轮 spawn 路径取出、保存并确认（可靠交接，不虚报成功）。
type NextTurnQueue = HashMap<String, VecDeque<Command>>;

static NEXT_TURN_QUEUE: OnceLock<Mutex<NextTurnQueue>> = OnceLock::new();

fn next_turn_queue() -> &'static Mutex<NextTurnQueue> {
    NEXT_TURN_QUEUE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 将命令排入指定会话的下一轮队列（封口期间 deliver 调用）。
/// 返回 `true` 表示成功入队；`false` 表示队列锁异常（Mutex poisoned），
/// 调用方应据此返回错误，不虚报成功（ALR-202）。
pub fn push_next_turn(session_id: &str, cmd: Command) -> bool {
    match next_turn_queue().lock() {
        Ok(mut queue) => {
            queue
                .entry(session_id.to_string())
                .or_default()
                .push_back(cmd);
            true
        }
        Err(_) => {
            // Mutex poisoned——不应发生，但必须如实报告，不能静默丢弃消息。
            false
        }
    }
}

/// 将未处理完的命令按原顺序放回指定会话下一轮队列的**前端**
/// （保存失败时恢复队列，保证消息不丢失、顺序不变，ALR-202）。
pub fn requeue_next_turn_front(session_id: &str, commands: Vec<Command>) {
    if commands.is_empty() {
        return;
    }
    if let Ok(mut queue) = next_turn_queue().lock() {
        let deque = queue.entry(session_id.to_string()).or_default();
        // 按原顺序插入到前端：从后往前 push_front 保持原始顺序。
        for cmd in commands.into_iter().rev() {
            deque.push_front(cmd);
        }
    }
}

/// 取出并清空指定会话的下一轮队列（下一轮 spawn 时调用）。
pub fn drain_next_turn(session_id: &str) -> Vec<Command> {
    if let Ok(mut queue) = next_turn_queue().lock()
        && let Some(pending) = queue.remove(session_id)
    {
        pending.into_iter().collect()
    } else {
        Vec::new()
    }
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
/// turn task 正常结束时，wrapper 会按任务代际立即清理。
/// panic 的任务保留在表中，供关闭路径等待并上报。
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
    let ingress = CommandIngress::new(cmd_tx);
    for plugin in &context.plugins {
        plugin.set_feedback_tx(PluginFeedbackTx::new(ingress.clone()));
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
            ingress,
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
/// 无活跃任务、任务已结束、或已进入终态封口（`Sealing`/`Committing`）时返回
/// false。封口期间投递被拒，调用方（deliver）应把用户消息排入下一轮队列。
pub fn send_command(session_id: &str, cmd: Command) -> bool {
    if let Ok(tasks) = turn_tasks().lock()
        && let Some(task) = tasks.get(session_id)
        && !task.handle.is_finished()
    {
        return task.ingress.send(cmd);
    }
    false
}

/// 终态封口：`Accepting → Sealing`（PendingFinish 提交前由执行线程调用）。
pub fn begin_seal(session_id: &str) {
    if let Ok(tasks) = turn_tasks().lock()
        && let Some(task) = tasks.get(session_id)
    {
        task.ingress.begin_seal();
    }
}

/// 封口排空发现继续命令：`Sealing → Accepting`，恢复接收。
pub fn reopen(session_id: &str) {
    if let Ok(tasks) = turn_tasks().lock()
        && let Some(task) = tasks.get(session_id)
    {
        task.ingress.reopen();
    }
}

/// 确定提交终态：`Sealing → Committing`。
pub fn commit_ingress(session_id: &str) {
    if let Ok(tasks) = turn_tasks().lock()
        && let Some(task) = tasks.get(session_id)
    {
        task.ingress.commit();
    }
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

/// 取消并等待指定 session 的活跃 turn 完成。
pub fn cancel_and_join(session_id: &str) -> Result<(), crate::core::CoreError> {
    let handle = {
        let mut tasks = turn_tasks()
            .lock()
            .map_err(|_| crate::core::CoreError::WorkerStopped)?;
        let Some(task) = tasks.remove(session_id) else {
            return Ok(());
        };
        // 关闭路径：强制取消必须送达（绕过门控——任务即将销毁，封口语义无意义）。
        let _ = task.ingress.force_send(Command::Cancel);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_gates_send_by_state() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        let ingress = CommandIngress::new(tx);
        assert!(ingress.is_accepting(), "新建 ingress 应为 Accepting");
        assert!(ingress.send(Command::Cancel), "Accepting 时应接受");
        assert!(matches!(rx.try_recv(), Ok(Command::Cancel)));

        ingress.begin_seal();
        assert!(!ingress.is_accepting(), "Sealing 后不应可接收");
        assert!(!ingress.send(Command::Cancel), "Sealing 后应拒绝");
        ingress.reopen();
        assert!(ingress.is_accepting(), "reopen 后恢复接受");
        assert!(ingress.send(Command::Cancel), "reopen 后恢复接受");
        ingress.begin_seal();
        ingress.commit();
        assert!(!ingress.is_accepting(), "Committing 后不应可接收");
        assert!(!ingress.send(Command::Cancel), "Committing 后应拒绝");
        // 已封口不可再 reopen（Committing 是终态方向）。
        ingress.reopen();
        assert!(
            !ingress.send(Command::Cancel),
            "Committing 不因 reopen 恢复"
        );
        assert!(!ingress.is_accepting(), "Committing 不因 reopen 恢复");
        // force_send 仍可送达（关闭路径专用）。
        assert!(ingress.force_send(Command::Cancel));
    }

    #[test]
    fn next_turn_queue_round_trip() {
        let sid = format!("next-turn-{}", scru128::new_string());
        assert!(drain_next_turn(&sid).is_empty(), "空队列应返回空");
        assert!(push_next_turn(&sid, Command::Cancel), "入队应返回 true");
        assert!(push_next_turn(&sid, Command::Shutdown), "入队应返回 true");
        let drained = drain_next_turn(&sid);
        assert_eq!(drained.len(), 2, "应按序取出全部排队命令");
        assert!(matches!(drained[0], Command::Cancel));
        assert!(matches!(drained[1], Command::Shutdown));
        assert!(drain_next_turn(&sid).is_empty(), "取出后队列应清空");
    }

    #[test]
    fn requeue_next_turn_front_preserves_order() {
        let sid = format!("next-turn-{}", scru128::new_string());
        // 先入队两条已处理（保留），再放回 [A, B, C] 到前端。
        push_next_turn(&sid, Command::Cancel);
        push_next_turn(&sid, Command::Shutdown);
        requeue_next_turn_front(
            &sid,
            vec![
                Command::SetTitle {
                    title: "A".to_string(),
                    only_if_default: false,
                },
                Command::SetTitle {
                    title: "B".to_string(),
                    only_if_default: false,
                },
                Command::SetTitle {
                    title: "C".to_string(),
                    only_if_default: false,
                },
            ],
        );
        let drained = drain_next_turn(&sid);
        assert_eq!(drained.len(), 5, "放回的命令应插入到已有队列之前");
        assert!(matches!(
            &drained[0],
            Command::SetTitle { title, .. } if title == "A"
        ));
        assert!(matches!(
            &drained[1],
            Command::SetTitle { title, .. } if title == "B"
        ));
        assert!(matches!(
            &drained[2],
            Command::SetTitle { title, .. } if title == "C"
        ));
        assert!(matches!(drained[3], Command::Cancel), "原有队列保持在其后");
        assert!(
            matches!(drained[4], Command::Shutdown),
            "原有队列保持在其后"
        );
    }

    /// 构造仅用于注册表测试的最小 TurnContext（不发真实请求）。
    fn dummy_context() -> (TurnContext, String) {
        use crate::agent_config::AgentConfig;
        use crate::model::SingleProviderClient;
        use tiangong_llm::ModelEndpoint;

        let mut session = crate::session::Session::new("ingress-test");
        let sid = session.id.clone();
        let root = tempfile::tempdir().expect("临时目录创建失败");
        session.bind_storage_root(root.path());
        std::mem::forget(root);
        let endpoint = ModelEndpoint {
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            ..Default::default()
        };
        let ctx = TurnContext::builder()
            .client(SingleProviderClient::new(endpoint))
            .session(session)
            .stream_tx(std::sync::mpsc::channel::<tiangong_types::StreamEvent>().0)
            .plugins(Vec::new())
            .context_limit(1_000)
            .agent_config(AgentConfig::default())
            .trust_mode(crate::permission::TrustMode::FullTrust)
            .observer(crate::observe::Observer::new(std::env::temp_dir()))
            .tools(Vec::new())
            .tool_overrides(std::collections::HashMap::new())
            .build();
        (ctx, sid)
    }

    /// ALR-201：注册表级封口门控——spawn_turn 注册的任务，封口前命令可投递，
    /// Sealing/Committing 后被拒；被拒的用户消息进入下一轮队列可靠交接。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registry_level_sealing_gates_send_command() {
        let (ctx, sid) = dummy_context();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        spawn_turn(ctx, move |_ctx, cmd_rx| {
            Ok(async move {
                // 持有接收端保持通道开启；挂起任务直到测试释放，保持注册表中
                // 的活跃代任务。
                let _cmd_rx = cmd_rx;
                let _ = release_rx.await;
            })
        })
        .expect("spawn 失败");

        // Accepting：命令可投递。
        assert!(
            send_command(
                &sid,
                Command::SetTitle {
                    title: "t".to_string(),
                    only_if_default: false,
                }
            ),
            "Accepting 时应可投递"
        );

        // Sealing：新命令被拒；用户消息进入下一轮队列。
        begin_seal(&sid);
        assert!(!send_command(&sid, Command::Cancel), "Sealing 后应拒绝投递");
        push_next_turn(
            &sid,
            Command::InjectUserMessage {
                message_id: "queued-1".to_string(),
                content: Vec::new(),
            },
        );

        // reopen：恢复接收。
        reopen(&sid);
        assert!(send_command(&sid, Command::Cancel), "reopen 后应恢复投递");

        // Committing：终态方向，不再恢复。
        begin_seal(&sid);
        commit_ingress(&sid);
        reopen(&sid);
        assert!(
            !send_command(&sid, Command::Cancel),
            "Committing 后应永久拒绝"
        );

        // 下一轮交接：队列中的消息可被取出。
        let queued = drain_next_turn(&sid);
        assert_eq!(queued.len(), 1, "封口期间的消息应在队列中");

        // 释放任务，注册表自清理。
        release_tx.send(()).expect("释放任务失败");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while is_running(&sid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!is_running(&sid), "任务应已结束并清理注册表");
    }
}
