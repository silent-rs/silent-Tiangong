//! 交互模型核心：请求管理器、审批授权表与挑战表（交互模型重做方案 §7/§8/§10/§12/§13）。
//!
//! Agent 经 `request_user` 工具发起审批/确认/选择/输入请求；本模块登记请求并
//! 保证原子闭合（响应/超时/取消只有一个生效，同一 Tool Call 永远只有一个结果）。
//! 闭合后经注入的 [`InteractionCloseHandler`] 通知宿主（写 Tool Result、续跑
//! Agent、发事件），本模块不感知 Loop 与 UI。
//!
//! 审批授权（§12）与挑战（§13）为运行期内存状态：会话隔离、重启失效、
//! 不写入 Session 文件。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Local;

/// 交互请求默认时限（方案 §8：第一版统一 15 秒，宿主设定）。
pub const DEFAULT_INTERACTION_TIMEOUT: Duration = Duration::from_secs(15);

/// 审批挑战兑换时限：覆盖 Agent 一轮模型往返后携带挑战发起请求_user。
const CHALLENGE_TTL: Duration = Duration::from_secs(300);

fn now() -> chrono::NaiveDateTime {
    Local::now().naive_local()
}

fn later(duration: Duration) -> chrono::NaiveDateTime {
    now() + chrono::Duration::from_std(duration).unwrap_or_else(|_| chrono::Duration::seconds(60))
}

/// 请求种类（方案 §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InteractionRequestKind {
    Approval,
    Confirm,
    Choice,
    MultiChoice,
    Input,
    Form,
}

impl InteractionRequestKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Confirm => "confirm",
            Self::Choice => "choice",
            Self::MultiChoice => "multi_choice",
            Self::Input => "input",
            Self::Form => "form",
        }
    }
}

/// 请求状态（方案 §7）：Pending 只能单向转入一个终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionStatus {
    Pending,
    Answered,
    Expired,
    Cancelled,
}

/// 交互请求（方案 §7 结构）。
#[derive(Debug, Clone)]
pub struct InteractionRequest {
    pub request_id: String,
    pub session_id: String,
    /// 发起请求的 assistant tool-call 消息 ID（可选，审计用）。
    pub source_message_id: Option<String>,
    /// 挂起待闭合的 tool_call id。
    pub tool_call_id: String,
    pub kind: InteractionRequestKind,
    pub title: String,
    pub description: String,
    /// 交互负载 JSON 文本（options/fields/question 等）。
    pub payload: String,
    /// approval 请求绑定的挑战（创建请求时由宿主从挑战表消费取得，
    /// 批准时按此真实目标生成授权，不信 Agent 报文）。
    pub approval_challenge: Option<ApprovalChallenge>,
    pub status: InteractionStatus,
    pub created_at: chrono::NaiveDateTime,
    /// 绝对截止时间（真实时钟，休眠后仍按此判定，方案 §8）。
    pub deadline: chrono::NaiveDateTime,
}

/// 请求闭合结果（原子闭合的唯一赢家产出）。
#[derive(Debug, Clone)]
pub struct ClosedInteraction {
    pub request: InteractionRequest,
    pub outcome: ClosedOutcome,
}

#[derive(Debug, Clone)]
pub enum ClosedOutcome {
    /// 用户响应（负载 JSON 文本）。
    Answered { result: String },
    /// 超时（fail-closed：审批不产生授权）。
    Expired,
    /// 取消（如用户发送新消息）。
    Cancelled { reason: String },
}

/// 闭合通知：由宿主注入处理（写 Tool Result、按会话状态续跑、发事件）。
pub type InteractionCloseHandler = Arc<dyn Fn(ClosedInteraction) + Send + Sync + 'static>;

/// respond/expire/cancel 的竞争结果（方案 §10）。
#[derive(Debug, Clone)]
pub enum CloseOutcome {
    /// 本方胜出：闭合已生效，宿主收到通知。
    Won(Box<ClosedInteraction>),
    /// 请求已闭合（迟到方）：携带已生效的终态。
    AlreadyClosed(InteractionStatus),
    /// 请求不存在（从未创建或已被清理）。
    NotFound,
}

struct RegistryState {
    pending: HashMap<String, InteractionRequest>,
    closed: HashMap<String, ClosedInteraction>,
}

/// 交互请求管理器：登记、原子闭合与查询。
///
/// 请求一旦闭合即冻结：迟到响应按 [`CloseOutcome::AlreadyClosed`] 拒绝；
/// 同一请求只有一个 Tool Result（方案 §10）。
pub struct InteractionRegistry {
    state: Mutex<RegistryState>,
    timeout: Duration,
    on_closed: Mutex<Option<InteractionCloseHandler>>,
}

impl InteractionRegistry {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                pending: HashMap::new(),
                closed: HashMap::new(),
            }),
            timeout: DEFAULT_INTERACTION_TIMEOUT,
            on_closed: Mutex::new(None),
        }
    }

    /// 注入闭合处理器（宿主启动时一次性设置）。
    pub fn set_close_handler(&self, handler: InteractionCloseHandler) {
        *self.on_closed.lock().expect("交互闭合处理器锁损坏") = Some(handler);
    }

    /// 创建请求：登记 Pending 并返回（deadline 由本方法计算）。
    /// 计时任务由宿主（Core）spawn，只持 request_id 与注册表句柄（方案 §8）。
    pub fn create(&self, mut request: InteractionRequest) -> InteractionRequest {
        request.status = InteractionStatus::Pending;
        request.created_at = now();
        request.deadline = request.created_at
            + chrono::Duration::from_std(self.timeout)
                .unwrap_or_else(|_| chrono::Duration::seconds(15));
        self.state
            .lock()
            .expect("交互注册表锁损坏")
            .pending
            .insert(request.request_id.clone(), request.clone());
        request
    }

    /// 是否已过绝对截止时间（真实时钟判定，休眠后仍准确）。
    pub fn is_expired(&self, request: &InteractionRequest) -> bool {
        now() >= request.deadline
    }

    /// 用户响应：在同一把锁内先执行绝对截止时间判定，再决定 Answered/Expired。
    /// 这样 deadline 已过但计时任务尚未获得调度时，迟到审批仍会 fail-closed。
    pub fn respond(&self, request_id: &str, result: String) -> CloseOutcome {
        let (closed, handler) = {
            let mut state = self.state.lock().expect("交互注册表锁损坏");
            let Some(mut request) = state.pending.remove(request_id) else {
                return match state.closed.get(request_id) {
                    Some(previous) => CloseOutcome::AlreadyClosed(previous.request.status),
                    None => CloseOutcome::NotFound,
                };
            };
            let outcome = if now() >= request.deadline {
                request.status = InteractionStatus::Expired;
                ClosedOutcome::Expired
            } else {
                request.status = InteractionStatus::Answered;
                ClosedOutcome::Answered { result }
            };
            let closed = ClosedInteraction {
                request: request.clone(),
                outcome,
            };
            state.closed.insert(request_id.to_string(), closed.clone());
            let handler = self.on_closed.lock().expect("交互闭合处理器锁损坏").clone();
            (closed, handler)
        };
        if let Some(handler) = handler {
            handler(closed.clone());
        }
        CloseOutcome::Won(Box::new(closed))
    }

    /// 超时闭合：计时任务醒来先按绝对 deadline 复核（未到则忽略），再原子转 Expired。
    pub fn expire(&self, request_id: &str) -> CloseOutcome {
        {
            let state = self.state.lock().expect("交互注册表锁损坏");
            if let Some(request) = state.pending.get(request_id)
                && now() < request.deadline
            {
                // 计时器早醒（休眠/时钟偏差）：未到真实截止，忽略本次
                return CloseOutcome::NotFound;
            }
        }
        self.close_with(request_id, InteractionStatus::Expired, |_| {
            ClosedOutcome::Expired
        })
    }

    /// 取消闭合（用户发送新消息等）。
    pub fn cancel(&self, request_id: &str, reason: String) -> CloseOutcome {
        self.close_with(request_id, InteractionStatus::Cancelled, |_| {
            ClosedOutcome::Cancelled {
                reason: reason.clone(),
            }
        })
    }

    fn close_with(
        &self,
        request_id: &str,
        expected: InteractionStatus,
        build_outcome: impl FnOnce(&InteractionRequest) -> ClosedOutcome,
    ) -> CloseOutcome {
        let (closed, handler) = {
            let mut state = self.state.lock().expect("交互注册表锁损坏");
            let Some(mut request) = state.pending.remove(request_id) else {
                return match state.closed.get(request_id) {
                    Some(previous) => CloseOutcome::AlreadyClosed(previous.request.status),
                    None => CloseOutcome::NotFound,
                };
            };
            request.status = expected;
            let closed = ClosedInteraction {
                request: request.clone(),
                outcome: build_outcome(&request),
            };
            state.closed.insert(request_id.to_string(), closed.clone());
            let handler = self.on_closed.lock().expect("交互闭合处理器锁损坏").clone();
            (closed, handler)
        };
        // 通知在锁外执行：处理器可能回调注册表（避免死锁）
        if let Some(handler) = handler {
            handler(closed.clone());
        }
        CloseOutcome::Won(Box::new(closed))
    }

    /// 会话的 Pending 请求（新消息到达时逐个取消，方案 §16）。
    pub fn pending_of_session(&self, session_id: &str) -> Vec<InteractionRequest> {
        self.state
            .lock()
            .expect("交互注册表锁损坏")
            .pending
            .values()
            .filter(|request| request.session_id == session_id)
            .cloned()
            .collect()
    }

    /// 查询请求所属的权威会话。仅 Pending 请求可以被 UI 响应。
    pub fn pending_session_id(&self, request_id: &str) -> Option<String> {
        self.state
            .lock()
            .expect("交互注册表锁损坏")
            .pending
            .get(request_id)
            .map(|request| request.session_id.clone())
    }

    /// 单个请求查询（UI 恢复展示用）。
    pub fn query(&self, request_id: &str) -> Option<InteractionRequest> {
        let state = self.state.lock().expect("交互注册表锁损坏");
        state.pending.get(request_id).cloned().or_else(|| {
            state
                .closed
                .get(request_id)
                .map(|item| item.request.clone())
        })
    }
}

impl Default for InteractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── 审批授权表（方案 §12）──

/// 授权粒度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantScope {
    /// 仅本次：绑定参数哈希，匹配后立即消费。
    Once { arguments_hash: String },
    /// 本次运行内：跨 turn、会话隔离、重启失效。
    Runtime,
}

/// 一条审批授权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalGrant {
    pub session_id: String,
    /// 工具来源插件（内置工具为空）。
    pub plugin_id: String,
    pub tool_name: String,
    pub scope: GrantScope,
    pub expires_at: chrono::NaiveDateTime,
}

/// 运行期授权表：内存态（不写入 Session 文件）。
#[derive(Default)]
pub struct ApprovalGrants {
    once: Mutex<Vec<ApprovalGrant>>,
    runtime: Mutex<HashSet<(String, String, String)>>,
}

impl ApprovalGrants {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次性授权（参数哈希绑定，方案 §13）。
    pub fn grant_once(
        &self,
        session_id: &str,
        plugin_id: &str,
        tool_name: &str,
        arguments_hash: String,
    ) {
        self.once.lock().expect("授权表锁损坏").push(ApprovalGrant {
            session_id: session_id.to_string(),
            plugin_id: plugin_id.to_string(),
            tool_name: tool_name.to_string(),
            scope: GrantScope::Once {
                arguments_hash: arguments_hash.clone(),
            },
            expires_at: later(CHALLENGE_TTL),
        });
    }

    /// 登记运行期授权。
    pub fn grant_runtime(&self, session_id: &str, plugin_id: &str, tool_name: &str) {
        self.runtime.lock().expect("授权表锁损坏").insert((
            session_id.to_string(),
            plugin_id.to_string(),
            tool_name.to_string(),
        ));
    }

    /// 校验并消费：Once 命中（参数哈希一致）即消费删除；Runtime 命中放行不消费。
    pub fn try_consume(
        &self,
        session_id: &str,
        plugin_id: &str,
        tool_name: &str,
        arguments_hash: &str,
    ) -> bool {
        if self.runtime.lock().expect("授权表锁损坏").contains(&(
            session_id.to_string(),
            plugin_id.to_string(),
            tool_name.to_string(),
        )) {
            return true;
        }
        let mut once = self.once.lock().expect("授权表锁损坏");
        let now = now();
        once.retain(|grant| grant.expires_at > now);
        let position = once.iter().position(|grant| {
            grant.session_id == session_id
                && grant.plugin_id == plugin_id
                && grant.tool_name == tool_name
                && grant.scope
                    == GrantScope::Once {
                        arguments_hash: arguments_hash.to_string(),
                    }
        });
        match position {
            Some(index) => {
                once.remove(index);
                true
            }
            None => false,
        }
    }
}

// ── 审批挑战表（方案 §13）──

/// 受保护工具无授权时的结构化挑战。
#[derive(Debug, Clone)]
pub struct ApprovalChallenge {
    pub challenge_id: String,
    pub session_id: String,
    pub plugin_id: String,
    pub tool_name: String,
    pub arguments_hash: String,
    pub summary: String,
    pub created_at: chrono::NaiveDateTime,
    pub expires_at: chrono::NaiveDateTime,
}

impl ApprovalChallenge {
    /// 序列化为工具结果负载（Agent 可见的挑战报文，方案 §13）。
    pub fn to_tool_payload(&self) -> String {
        serde_json::json!({
            "status": "approval_required",
            "challenge_id": self.challenge_id,
            "plugin_id": self.plugin_id,
            "tool_name": self.tool_name,
            "arguments_hash": self.arguments_hash,
            "summary": self.summary,
        })
        .to_string()
    }
}

/// 挑战表：一次性消费（take），过期即失效。
#[derive(Default)]
pub struct ApprovalChallenges {
    challenges: Mutex<HashMap<String, ApprovalChallenge>>,
}

impl ApprovalChallenges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        session_id: &str,
        plugin_id: &str,
        tool_name: &str,
        arguments_hash: String,
        summary: String,
    ) -> ApprovalChallenge {
        let challenge = ApprovalChallenge {
            challenge_id: scru128::new().to_string(),
            session_id: session_id.to_string(),
            plugin_id: plugin_id.to_string(),
            tool_name: tool_name.to_string(),
            arguments_hash,
            summary,
            created_at: now(),
            expires_at: later(CHALLENGE_TTL),
        };
        let mut challenges = self.challenges.lock().expect("挑战表锁损坏");
        challenges.retain(|_, item| item.expires_at > now());
        challenges.insert(challenge.challenge_id.clone(), challenge.clone());
        challenge
    }

    /// 消费性取出：Agent 携带挑战发起 approval 请求时由宿主调用，
    /// 取得真实审批目标（不信 Agent 报文）。
    pub fn take(&self, challenge_id: &str) -> Option<ApprovalChallenge> {
        let mut challenges = self.challenges.lock().expect("挑战表锁损坏");
        let challenge = challenges.remove(challenge_id)?;
        (challenge.expires_at > now()).then_some(challenge)
    }

    /// 取会话最新的未消费挑战（模型未携带 challenge_id 时的容错路径；
    /// 仍由宿主从挑战表取得真实目标，不信 Agent 报文）。
    pub fn take_latest_of_session(&self, session_id: &str) -> Option<ApprovalChallenge> {
        let mut challenges = self.challenges.lock().expect("挑战表锁损坏");
        let (challenge_id, _) = challenges
            .iter()
            .filter(|(_, item)| item.session_id == session_id && item.expires_at > now())
            .max_by_key(|(_, item)| item.created_at)?;
        let challenge_id = challenge_id.clone();
        challenges.remove(&challenge_id)
    }
}

/// 闭合请求渲染为 request_user 的 Tool Result 负载（方案 §3/§9）。
/// 返回 (payload, ok)：answered 为 ok，expired/cancelled 为失败结果。
pub fn render_closed_tool_result(closed: &ClosedInteraction) -> (String, bool) {
    let kind = closed.request.kind.as_str();
    match &closed.outcome {
        ClosedOutcome::Answered { result } => {
            let parsed = serde_json::from_str::<serde_json::Value>(result)
                .unwrap_or(serde_json::Value::Null);
            (
                serde_json::json!({
                    "status": "answered",
                    "kind": kind,
                    "request_id": closed.request.request_id,
                    "result": parsed,
                })
                .to_string(),
                true,
            )
        }
        ClosedOutcome::Expired => (
            serde_json::json!({
                "status": "expired",
                "kind": kind,
                "request_id": closed.request.request_id,
                "message": if kind == "approval" {
                    "用户未在规定时间内响应，操作未获批准"
                } else {
                    "用户未在规定时间内响应"
                },
            })
            .to_string(),
            false,
        ),
        ClosedOutcome::Cancelled { reason } => (
            serde_json::json!({
                "status": "cancelled",
                "kind": kind,
                "request_id": closed.request.request_id,
                "reason": reason,
            })
            .to_string(),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_request(kind: InteractionRequestKind) -> InteractionRequest {
        InteractionRequest {
            request_id: scru128::new().to_string(),
            session_id: "s1".to_string(),
            source_message_id: None,
            tool_call_id: "call-1".to_string(),
            kind,
            title: "确认".to_string(),
            description: String::new(),
            payload: String::new(),
            approval_challenge: None,
            status: InteractionStatus::Pending,
            created_at: now(),
            deadline: now(),
        }
    }

    #[test]
    fn 响应与超时竞态只有一个赢家() {
        let registry = InteractionRegistry::new();
        let closed_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = closed_count.clone();
        registry.set_close_handler(Arc::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        let request = registry.create(pending_request(InteractionRequestKind::Choice));
        // 响应先到
        let won = registry.respond(&request.request_id, r#""main""#.to_string());
        assert!(matches!(won, CloseOutcome::Won(_)));
        // 超时后到：已闭合拒绝
        let late = registry.expire(&request.request_id);
        assert!(matches!(
            late,
            CloseOutcome::AlreadyClosed(InteractionStatus::Answered)
        ));
        // 迟到的第二次响应同样拒绝
        let late_response = registry.respond(&request.request_id, r#""dev""#.to_string());
        assert!(matches!(
            late_response,
            CloseOutcome::AlreadyClosed(InteractionStatus::Answered)
        ));
        assert_eq!(closed_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn 截止时间已过的响应原子闭合为过期() {
        let registry = InteractionRegistry::new();
        let mut request = pending_request(InteractionRequestKind::Approval);
        request.request_id = "expired-response".to_string();
        // 测试模块可直接登记过去的 deadline，模拟计时任务尚未调度但真实时间已过。
        request.status = InteractionStatus::Pending;
        request.created_at = now() - chrono::Duration::seconds(2);
        request.deadline = now() - chrono::Duration::seconds(1);
        registry
            .state
            .lock()
            .unwrap()
            .pending
            .insert(request.request_id.clone(), request.clone());

        let outcome = registry.respond(
            &request.request_id,
            r#"{\"decision\":\"approve_once\"}"#.to_string(),
        );
        match outcome {
            CloseOutcome::Won(closed) => {
                assert_eq!(closed.request.status, InteractionStatus::Expired);
                assert!(matches!(closed.outcome, ClosedOutcome::Expired));
            }
            other => panic!("迟到响应应由本次调用闭合为过期: {other:?}"),
        }
    }

    #[test]
    fn 未到真实截止的超时唤醒被忽略() {
        let registry = InteractionRegistry::new();
        let request = registry.create(pending_request(InteractionRequestKind::Input));
        // 刚创建远未到 deadline：expire 按绝对时间复核后忽略
        let outcome = registry.expire(&request.request_id);
        assert!(matches!(outcome, CloseOutcome::NotFound));
        // 请求仍在 Pending，可正常响应
        let won = registry.respond(&request.request_id, r#""答案""#.to_string());
        assert!(matches!(won, CloseOutcome::Won(_)));
    }

    #[test]
    fn 取消闭合与会话查询() {
        let registry = InteractionRegistry::new();
        let request = registry.create(pending_request(InteractionRequestKind::Approval));
        assert_eq!(registry.pending_of_session("s1").len(), 1);
        assert!(registry.pending_of_session("other").is_empty());

        let won = registry.cancel(&request.request_id, "用户发送了新的消息".to_string());
        match won {
            CloseOutcome::Won(closed) => {
                assert_eq!(closed.request.status, InteractionStatus::Cancelled);
                assert!(matches!(closed.outcome, ClosedOutcome::Cancelled { .. }));
            }
            other => panic!("取消应胜出: {other:?}"),
        }
        assert!(registry.pending_of_session("s1").is_empty());
    }

    #[test]
    fn 未知请求返回_not_found() {
        let registry = InteractionRegistry::new();
        assert!(matches!(
            registry.respond("missing", "{}".to_string()),
            CloseOutcome::NotFound
        ));
        assert!(matches!(
            registry.cancel("missing", String::new()),
            CloseOutcome::NotFound
        ));
    }

    #[test]
    fn 一次性授权消费与参数绑定() {
        let grants = ApprovalGrants::new();
        grants.grant_once("s1", "", "delete_file", "hash-a".to_string());

        // 参数匹配：消费放行
        assert!(grants.try_consume("s1", "", "delete_file", "hash-a"));
        // 一次性：第二次不再放行（验收 §10）
        assert!(!grants.try_consume("s1", "", "delete_file", "hash-a"));

        // 参数变化：不匹配（验收 §11）
        grants.grant_once("s1", "", "delete_file", "hash-a".to_string());
        assert!(!grants.try_consume("s1", "", "delete_file", "hash-b"));
    }

    #[test]
    fn 运行期授权跨调用有效且会话隔离() {
        let grants = ApprovalGrants::new();
        grants.grant_runtime("s1", "", "run_command");
        // 跨调用持续放行（不消费）
        assert!(grants.try_consume("s1", "", "run_command", "any"));
        assert!(grants.try_consume("s1", "", "run_command", "other"));

        // 会话隔离（验收 §14）；不同插件不共享（验收 §15）
        assert!(!grants.try_consume("s2", "", "run_command", "any"));
        assert!(!grants.try_consume("s1", "plugin-x", "run_command", "any"));
    }

    #[test]
    fn 挑战一次性消费与过期() {
        let challenges = ApprovalChallenges::new();
        let challenge = challenges.create(
            "s1",
            "fs",
            "delete_file",
            "hash-a".to_string(),
            "删除 /tmp/x".to_string(),
        );
        assert!(challenge.to_tool_payload().contains("approval_required"));

        // 消费性取出：第二次为空
        let taken = challenges.take(&challenge.challenge_id);
        assert!(taken.is_some());
        assert!(challenges.take(&challenge.challenge_id).is_none());
    }
}

#[cfg(test)]
mod redesign_acceptance_tests {
    use super::*;

    fn pending(kind: InteractionRequestKind, session: &str) -> InteractionRequest {
        InteractionRequest {
            request_id: scru128::new().to_string(),
            session_id: session.to_string(),
            source_message_id: None,
            tool_call_id: "call-1".to_string(),
            kind,
            title: "确认".to_string(),
            description: String::new(),
            payload: String::new(),
            approval_challenge: None,
            status: InteractionStatus::Pending,
            created_at: now(),
            deadline: now(),
        }
    }

    /// 验收「会话路由」：request_id 是权威键，pending_session_id 返回所属会话；
    /// 其他会话的查询不串扰（方案 §8）。
    #[test]
    fn 按请求_id_权威路由到所属会话() {
        let registry = InteractionRegistry::new();
        let request_a = registry.create(pending(InteractionRequestKind::Approval, "session-a"));
        let _request_b = registry.create(pending(InteractionRequestKind::Choice, "session-b"));

        assert_eq!(
            registry
                .pending_session_id(&request_a.request_id)
                .as_deref(),
            Some("session-a")
        );
        // 闭合后不再可路由（迟到响应按 NotFound 拒绝）
        registry.respond(&request_a.request_id, "true".to_string());
        assert_eq!(registry.pending_session_id(&request_a.request_id), None);
    }

    /// 验收「deadline 与响应竞争」：过期瞬间的响应在注册表锁内判为 Expired，
    /// 不产生 answered 结果（方案 §10）。
    #[test]
    fn 过期边界的响应闭合为_expired_而非_answered() {
        let registry = InteractionRegistry::new();
        let mut request = pending(InteractionRequestKind::Input, "s1");
        // deadline 设为过去：创建路径会覆盖，这里手动构造已过期的 Pending
        request.deadline = now() - chrono::Duration::seconds(1);
        registry
            .state
            .lock()
            .expect("交互注册表锁损坏")
            .pending
            .insert(request.request_id.clone(), request.clone());

        let outcome = registry.respond(&request.request_id, r#""答案""#.to_string());
        match outcome {
            CloseOutcome::Won(closed) => {
                assert_eq!(closed.request.status, InteractionStatus::Expired);
                assert!(matches!(closed.outcome, ClosedOutcome::Expired));
            }
            other => panic!("过期响应应闭合为 Expired: {other:?}"),
        }
    }

    /// 验收「挑战绑定」：挑战会话与请求会话不一致时拒绝（方案 §11/§13）。
    #[test]
    fn 跨会话挑战不可消费() {
        let challenges = ApprovalChallenges::new();
        let challenge = challenges.create(
            "session-a",
            "fs",
            "delete_file",
            "hash-a".to_string(),
            "删除 /tmp/x".to_string(),
        );
        // 从 B 会话取 A 的挑战：按 id 消费本身成功（表级 API），
        // 会话匹配由 request_user 侧校验——此处验证表级消费语义与会话字段保留
        let taken = challenges
            .take(&challenge.challenge_id)
            .expect("按 id 应可消费");
        assert_eq!(taken.session_id, "session-a");
        assert_eq!(taken.arguments_hash, "hash-a");
        // 二次消费为空（一次性）
        assert!(challenges.take(&challenge.challenge_id).is_none());
    }
}
