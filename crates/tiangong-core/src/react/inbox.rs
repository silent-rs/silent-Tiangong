//! Agent 自有 Inbox 与唯一 driver 的调度状态（ALR-001/101~106/201~206）。
//!
//! 每个 session 在注册表中持有一个 [`AgentScheduling`]：`next_turn` FIFO、
//! `next_step` 积压、`Idle | Running` 相位与唤醒锁存。同一 Agent 至多存在一个
//! driver 任务；所有状态变更在短临界区内完成，driver 不在持锁状态下执行
//! 模型、工具或 Session I/O。
//!
//! 本模块只做调度判定（入队、领取、唤醒、关闭），不执行任何 turn 逻辑——
//! 每轮的构建与执行由 core 层的 `drive_agent` 循环完成。

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::core::command::Command;

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

/// Agent Inbox 的 turn 级输入：一个完整活动，由唯一 driver 逐个领取执行。
pub(crate) enum TurnInput {
    /// followup 用户消息：保存为最新用户消息并执行一轮（ALR-101）。
    UserMessage {
        message_id: String,
        content: Vec<tiangong_types::ContentBlock>,
    },
    /// 空闲期手动上下文压缩（仅 Idle 接受，Busy 时投递方收到拒绝）。
    ManualCompression,
    /// 空闲期清理上下文（仅 Idle 接受）。
    ResetContext,
}

/// driver 对外相位（ALR-004：Agent 对外只有 `Idle | Running`）。
enum Phase {
    /// driver 挂起等待唤醒；`Notify` permit 充当唤醒锁存（ALR-105）。
    Idle,
    /// driver 活跃：执行 turn，或正处于领取下一项 / 关闭判定的循环边界。
    Running,
}

/// Agent 调度状态（短临界区内读写，不在此执行任何 I/O）。
struct SchedulingState {
    /// 是否仍接受新输入；关闭后置 false（ALR-206：先停止接收再收敛）。
    accepting: bool,
    phase: Phase,
    /// followup FIFO（ALR-101/205）。
    next_turn: VecDeque<TurnInput>,
    /// 无活动轮时积压的 inject 类输入；下一个 turn 开始时由 driver 一次性领取。
    next_step: VecDeque<Command>,
    /// 当前轮的命令入口；turn 结束后置 None。
    ingress: Option<CommandIngress>,
    /// driver 任务句柄（关闭路径 join 用）。
    driver: Option<JoinHandle<()>>,
}

impl SchedulingState {
    fn new() -> Self {
        Self {
            accepting: true,
            phase: Phase::Idle,
            next_turn: VecDeque::new(),
            next_step: VecDeque::new(),
            ingress: None,
            driver: None,
        }
    }
}

/// 单个 Agent（session）的调度实体：Inbox + 唤醒锁存 + 当前轮命令入口。
///
/// 唤醒锁存（ALR-105）由 `Phase::Idle` 与 `Notify` permit 共同保证：park 判定
/// 与投递方唤醒判定互斥，permit 覆盖「try_park 之后、notified().await 之前」
/// 到达的唤醒，不丢失也不重复启动。
pub(crate) struct AgentScheduling {
    state: Mutex<SchedulingState>,
    wake: Notify,
}

impl AgentScheduling {
    fn new() -> Self {
        Self {
            state: Mutex::new(SchedulingState::new()),
            wake: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SchedulingState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// 入队一个 turn 级输入并按需唤醒 driver（投递与唤醒同临界区，ALR-105）。
    pub(crate) fn push_turn_input(&self, input: TurnInput) -> Result<(), crate::core::CoreError> {
        let mut state = self.lock();
        if !state.accepting {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        state.next_turn.push_back(input);
        self.wake_if_idle(&mut state);
        Ok(())
    }

    /// steer 语义（ALR-102）：有活动轮时投递到当前轮（修正当前意图），
    /// 无活动轮或当前轮正在封口时进入 `next_turn`，等同 followup（不丢消息）。
    pub(crate) fn push_steer(
        &self,
        message_id: String,
        content: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(), crate::core::CoreError> {
        let mut state = self.lock();
        if !state.accepting {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        let steered = state.ingress.as_ref().is_some_and(|ingress| {
            ingress.send(Command::InjectUserMessage {
                message_id: message_id.clone(),
                content: content.clone(),
            })
        });
        if !steered {
            state.next_turn.push_back(TurnInput::UserMessage {
                message_id,
                content,
            });
            self.wake_if_idle(&mut state);
        }
        Ok(())
    }

    /// inject 语义（ALR-102）：有活动轮时投递到当前轮；否则积压在 `next_step`，
    /// 由下一个 turn 开始时领取（不主动唤醒，不阻止 driver 挂起）。
    pub(crate) fn push_inject(
        &self,
        tool_name: String,
        payload: serde_json::Value,
    ) -> Result<(), crate::core::CoreError> {
        let mut state = self.lock();
        if !state.accepting {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        let delivered = state.ingress.as_ref().is_some_and(|ingress| {
            ingress.send(Command::InjectTool {
                tool_name: tool_name.clone(),
                payload: payload.clone(),
            })
        });
        if !delivered {
            state
                .next_step
                .push_back(Command::InjectTool { tool_name, payload });
        }
        Ok(())
    }

    /// driver 领取下一个 turn 输入（FIFO）。
    pub(crate) fn take_next_turn(&self) -> Option<TurnInput> {
        self.lock().next_turn.pop_front()
    }

    /// turn 开始时领取当前积压的全部 `next_step`（保持接收顺序，ALR-103）。
    pub(crate) fn take_next_steps(&self) -> Vec<Command> {
        self.lock().next_step.drain(..).collect()
    }

    /// driver 尝试挂起（进入 Idle）。返回 true 表示应等待唤醒。
    ///
    /// 与投递方互斥：临界区内再次检查 `next_turn`，有待执行输入则不挂起，
    /// 由 driver 重新领取——取消收敛窗口到达的消息不会丢失（ALR-105）。
    /// `next_step` 积压不阻止挂起：inject 本就等待下一次自然活动。
    pub(crate) fn try_park(&self) -> bool {
        let mut state = self.lock();
        if state.next_turn.is_empty() {
            state.phase = Phase::Idle;
            true
        } else {
            false
        }
    }

    /// 挂起等待唤醒（相位已由唤醒方在临界区内切回 Running）。
    pub(crate) async fn wait_wake(&self) {
        self.wake.notified().await;
    }

    /// turn 执行前登记当前轮命令入口（对外切换为 Running）。
    pub(crate) fn begin_turn(&self, ingress: CommandIngress) {
        let mut state = self.lock();
        state.phase = Phase::Running;
        state.ingress = Some(ingress);
    }

    /// turn 结束后清理命令入口；相位保持 Running 直到 driver 回到循环边界
    /// （领取下一项或 try_park），期间对外至多短暂多报一次活跃。
    pub(crate) fn end_turn(&self) {
        self.lock().ingress = None;
    }

    /// 是否仍有未处理的 Inbox 输入（测试与诊断用）。
    #[cfg(test)]
    pub(crate) fn has_pending(&self) -> bool {
        let state = self.lock();
        !state.next_turn.is_empty() || !state.next_step.is_empty()
    }

    /// 是否仍接受新输入（driver 关闭分支与投递方判定共用）。
    pub(crate) fn is_accepting(&self) -> bool {
        self.lock().accepting
    }

    /// 取出全部未处理输入（关闭排空用；保持接收顺序）。
    pub(crate) fn drain_pending(&self) -> Vec<TurnInput> {
        self.lock().next_turn.drain(..).collect()
    }

    /// 关闭：停止接收新输入，取消当前轮并唤醒挂起的 driver（ALR-206）。
    /// 未处理输入由 driver 的关闭分支持久化或明确失败，不静默丢弃。
    pub(crate) fn shutdown(&self) -> Option<JoinHandle<()>> {
        let mut state = self.lock();
        state.accepting = false;
        if let Some(ingress) = state.ingress.as_ref() {
            // 关闭路径：强制取消必须送达（绕过门控——任务即将销毁）。
            ingress.force_send(Command::Cancel);
        }
        self.wake_if_idle(&mut state);
        state.driver.take()
    }

    fn wake_if_idle(&self, state: &mut SchedulingState) {
        if matches!(state.phase, Phase::Idle) {
            state.phase = Phase::Running;
            self.wake.notify_one();
        }
    }
}

/// Agent 注册表：session_id → 调度实体。条目在关闭后移除，重建 Core 时可重新创建。
type AgentMap = HashMap<String, Arc<AgentScheduling>>;

static AGENTS: OnceLock<Mutex<AgentMap>> = OnceLock::new();

fn agents() -> &'static Mutex<AgentMap> {
    AGENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 确保 session 的 Agent 调度实体存在，且**至多启动一个 driver**（ALR-001）。
///
/// `driver_factory` 只在实体首次创建或既有实体缺少 driver 时调用一次；
/// 并发调用由注册表锁串行化，不会重复启动。已关闭的实体返回错误，
/// 调用方应视为 Worker 已停止。
pub(crate) fn ensure_agent_session<F, Fut>(
    session_id: &str,
    driver_factory: F,
) -> Result<Arc<AgentScheduling>, crate::core::CoreError>
where
    F: FnOnce(Arc<AgentScheduling>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut agents = agents()
        .lock()
        .map_err(|_| crate::core::CoreError::WorkerStopped)?;
    if let Some(entry) = agents.get(session_id) {
        let state = entry.lock();
        if !state.accepting {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        if state.driver.is_some() {
            return Ok(entry.clone());
        }
    }
    // 无条目则创建；已有条目（如 ensure_inbox 先建立的 Inbox 载体）必须复用，
    // 保留其中积压的 next_step 输入，不得覆盖丢弃。
    let entry = match agents.get(session_id) {
        Some(entry) => entry.clone(),
        None => {
            let entry = Arc::new(AgentScheduling::new());
            agents.insert(session_id.to_string(), entry.clone());
            entry
        }
    };
    let future = driver_factory(entry.clone());
    let driver = crate::shared_runtime::shared_runtime().spawn(future);
    {
        let mut state = entry.lock();
        state.driver = Some(driver);
    }
    Ok(entry)
}

/// 仅确保 Inbox 载体存在（不启动 driver）：空闲期 inject 等待下一次自然
/// 活动时消费（ALR-102：inject 不唤醒）。已关闭的会话返回错误。
pub(crate) fn ensure_inbox(
    session_id: &str,
) -> Result<Arc<AgentScheduling>, crate::core::CoreError> {
    let mut agents = agents()
        .lock()
        .map_err(|_| crate::core::CoreError::WorkerStopped)?;
    if let Some(entry) = agents.get(session_id) {
        if !entry.lock().accepting {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        return Ok(entry.clone());
    }
    let entry = Arc::new(AgentScheduling::new());
    agents.insert(session_id.to_string(), entry);
    Ok(agents
        .get(session_id)
        .cloned()
        .expect("刚插入的条目必然存在"))
}

/// driver 正常退出时移除注册表条目（与 shutdown 路径互斥幂等）。
pub(crate) fn remove_agent(session_id: &str) {
    if let Ok(mut agents) = agents().lock() {
        agents.remove(session_id);
    }
}

/// 向当前活跃 turn 发送命令（取消、审批、工具注入和运行配置更新等）。
///
/// 无活跃 turn 或已进入终态封口（`Sealing`/`Committing`）时返回 false。
/// 调用方按语义处理：用户消息/工具注入应转入 Inbox，取消与审批可视为无操作。
pub fn send_command(session_id: &str, cmd: Command) -> bool {
    if let Ok(agents) = agents().lock()
        && let Some(entry) = agents.get(session_id)
    {
        let state = entry.lock();
        return state
            .ingress
            .as_ref()
            .is_some_and(|ingress| ingress.send(cmd));
    }
    false
}

/// 终态封口：`Accepting → Sealing`（PendingFinish 提交前由执行线程调用）。
pub fn begin_seal(session_id: &str) {
    if let Ok(agents) = agents().lock()
        && let Some(entry) = agents.get(session_id)
    {
        let state = entry.lock();
        if let Some(ingress) = state.ingress.as_ref() {
            ingress.begin_seal();
        }
    }
}

/// 封口排空发现继续命令：`Sealing → Accepting`，恢复接收。
pub fn reopen(session_id: &str) {
    if let Ok(agents) = agents().lock()
        && let Some(entry) = agents.get(session_id)
    {
        let state = entry.lock();
        if let Some(ingress) = state.ingress.as_ref() {
            ingress.reopen();
        }
    }
}

/// 确定提交终态：`Sealing → Committing`。
pub fn commit_ingress(session_id: &str) {
    if let Ok(agents) = agents().lock()
        && let Some(entry) = agents.get(session_id)
    {
        let state = entry.lock();
        if let Some(ingress) = state.ingress.as_ref() {
            ingress.commit();
        }
    }
}

/// 查询指定 session 是否有活跃 turn（对外 `Running`）。
pub fn is_running(session_id: &str) -> bool {
    if let Ok(agents) = agents().lock()
        && let Some(entry) = agents.get(session_id)
    {
        return matches!(entry.lock().phase, Phase::Running);
    }
    false
}

/// 查询指定 session 是否可继续接收输入（存在未关闭的调度实体）。
pub fn is_alive(session_id: &str) -> bool {
    if let Ok(agents) = agents().lock() {
        return agents
            .get(session_id)
            .is_some_and(|entry| entry.lock().accepting);
    }
    false
}

/// 关闭指定 session 的 Agent：停止接收、取消当前轮、唤醒挂起 driver，
/// 并等待 driver 完成关闭收敛（未处理输入的持久化由 driver 侧完成）。
pub fn shutdown_agent(session_id: &str) -> Result<(), crate::core::CoreError> {
    let driver = {
        let mut agents = agents()
            .lock()
            .map_err(|_| crate::core::CoreError::WorkerStopped)?;
        let Some(entry) = agents.remove(session_id) else {
            return Ok(());
        };
        entry.shutdown()
    };
    let Some(driver) = driver else {
        return Ok(());
    };
    std::thread::scope(|scope| {
        scope
            .spawn(move || crate::shared_runtime::shared_runtime().block_on(driver))
            .join()
            .map_err(|_| crate::core::CoreError::WorkerPanicked)?
            .map_err(|_| crate::core::CoreError::WorkerPanicked)
    })
}

/// 非阻塞关闭（Drop 路径）：停止接收、取消当前轮并唤醒 driver 自行收敛退出。
/// 不等待 driver 完成；driver 会自行持久化未处理输入后退出并清理注册表。
pub fn detach_shutdown(session_id: &str) {
    let driver = {
        let Ok(mut agents) = agents().lock() else {
            return;
        };
        let Some(entry) = agents.remove(session_id) else {
            return;
        };
        entry.shutdown()
    };
    drop(driver);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

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

    /// 单 driver 唤醒语义：FIFO 领取、park/唤醒互斥、关闭后拒绝（ALR-001/105）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inbox_delivers_turns_in_fifo_with_single_driver() {
        let sid = format!("inbox-fifo-{}", scru128::new_string());
        let entry = ensure_inbox(&sid).expect("创建 Inbox 失败");

        for i in 0..3 {
            entry
                .push_turn_input(TurnInput::UserMessage {
                    message_id: format!("msg-{i}"),
                    content: vec![tiangong_types::ContentBlock::text(format!("第 {i} 条"))],
                })
                .expect("投递应被接受");
        }
        // Inbox 无 driver 时不消费；领取保持 FIFO。
        let first = entry.take_next_turn().expect("应可领取");
        assert!(
            matches!(&first, TurnInput::UserMessage { message_id, .. } if message_id == "msg-0"),
            "FIFO 顺序领取第一条"
        );
        assert!(entry.has_pending(), "剩余输入仍在 Inbox");

        // park 语义：next_turn 有积压时不允许挂起；排空后允许
        //（next_step 积压不阻止挂起——inject 等待下次活动）。
        assert!(!entry.try_park(), "next_turn 有积压时不得挂起");
        entry
            .push_inject("browser".to_string(), serde_json::json!({}))
            .expect("inject 应被接受");
        let _ = entry.take_next_turn();
        let _ = entry.take_next_turn();
        assert!(
            entry.try_park(),
            "next_turn 排空后应允许挂起（next_step 积压不阻止）"
        );
        assert_eq!(
            entry.take_next_steps().len(),
            1,
            "park 前积压的 inject 保留在 next_step"
        );

        // 挂起中投递：唤醒锁存生效（Idle → 唤醒）。
        entry
            .push_turn_input(TurnInput::UserMessage {
                message_id: "msg-wake".to_string(),
                content: Vec::new(),
            })
            .expect("投递应被接受");
        let woken = tokio::time::timeout(std::time::Duration::from_secs(1), entry.wait_wake())
            .await
            .is_ok();
        assert!(woken, "挂起中的投递必须唤醒 driver");
        assert!(
            matches!(
                &entry.take_next_turn().expect("唤醒后领取"),
                TurnInput::UserMessage { message_id, .. } if message_id == "msg-wake"
            ),
            "唤醒后应领取到新输入"
        );

        // 关闭排空：shutdown 后不再接受。
        let _ = entry.shutdown();
        assert!(
            entry.push_turn_input(TurnInput::ManualCompression).is_err(),
            "关闭后应拒绝新输入"
        );
        remove_agent(&sid);
    }
}
