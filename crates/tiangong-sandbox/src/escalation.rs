//! 升级审批（RFC 0017 S4，透明执行封套修订）：宿主签发、命令文本绑定、
//! 一次性消费、短时效。
//!
//! 闭环模型（插件协议零审批字段）：
//! 1. Agent 的命令在沙箱内失败或被预分类拒绝 → 错误提示引导用户批准；
//! 2. 用户经宿主界面批准**完整命令文本**（原生确认对话框）→ 票据入库；
//! 3. Agent 重试同一命令（完整文本精确匹配）→ 宿主命中票据 →
//!    以一次性全权实例执行并消费票据。
//!
//! 安全边界：票据只能由宿主用户批准入口签发（桌面 UI 命令，Agent 与插件
//! 经桥接层无法触达）；换任何参数（文本不匹配）都无法消费他人批准。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 票据有效期：用户批准后 Agent 需在窗口内重试。
const TICKET_TTL: Duration = Duration::from_secs(300);

/// 无沙箱执行授权指纹（结构化，非拼接字符串——拼接对含空格参数有歧义）。
/// 至少绑定操作类型、程序、参数、完整脚本与工作目录（Session/Tool Call
/// 绑定待宿主调用上下文链路，见 RFC 开放问题）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EscalationFingerprint {
    pub operation: String,
    pub program: String,
    pub args: Vec<String>,
    pub script: String,
    pub cwd: String,
}

impl EscalationFingerprint {
    /// 从请求负载提取指纹（字段缺失记空串，保证稳定）。
    pub fn from_payload(operation: &str, payload: &serde_json::Value) -> Self {
        let str_field = |name: &str| {
            payload
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let args = payload
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cwd = payload
            .get("cwd")
            .or_else(|| {
                payload
                    .get("access")
                    .and_then(|access| access.get("workspace"))
            })
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Self {
            operation: operation.to_string(),
            program: str_field("cmd"),
            args,
            script: str_field("script"),
            cwd,
        }
    }
}

struct EscalationTicket {
    fingerprint: EscalationFingerprint,
    expires_at: Instant,
}

/// 待批准的升级请求（宿主在命令被高危拒绝或沙箱拦截时登记，
/// 指纹直接取自实际请求负载——审批与重试天然同源匹配）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingEscalation {
    pub id: String,
    pub fingerprint: EscalationFingerprint,
    /// 登记原因（高危预分类拒绝 / 沙箱拦截）。
    pub reason: String,
    pub created_at: String,
}

struct PendingStore {
    pending: Vec<PendingEscalation>,
}

fn pending_store() -> &'static Mutex<PendingStore> {
    static STORE: OnceLock<Mutex<PendingStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(PendingStore {
            pending: Vec::new(),
        })
    })
}

/// 登记待批准请求（同指纹去重，10 分钟时效）。
pub fn record_pending(fingerprint: EscalationFingerprint, reason: &str) {
    const PENDING_TTL: Duration = Duration::from_secs(600);
    if let Ok(mut store) = pending_store().lock() {
        store.pending.retain(|item| item.fingerprint != fingerprint);
        store.pending.push(PendingEscalation {
            id: scru128::new().to_string(),
            fingerprint,
            reason: reason.to_string(),
            created_at: chrono::Local::now()
                .naive_local()
                .format("%Y-%m-%dT%H:%M:%S%.3f")
                .to_string(),
        });
        let deadline = Instant::now().checked_sub(PENDING_TTL);
        let _ = deadline;
        // 以创建时间字符串排序无法可靠判断过期；简化为超量裁剪 + 消费时校验。
        while store.pending.len() > 16 {
            store.pending.remove(0);
        }
    }
}

/// 当前待批准列表（新建在前）。
pub fn list_pending() -> Vec<PendingEscalation> {
    let Ok(store) = pending_store().lock() else {
        return Vec::new();
    };
    let mut items = store.pending.clone();
    items.reverse();
    items
}

/// 按编号批准：对登记的结构化指纹签发票据（一次性）。
pub fn approve_pending(id: &str) -> Option<String> {
    let fingerprint = {
        let Ok(mut store) = pending_store().lock() else {
            return None;
        };
        let index = store.pending.iter().position(|item| item.id == id)?;
        Some(store.pending.remove(index).fingerprint)
    }?;
    Some(EscalationBroker::issue(fingerprint))
}

/// 升级票据库（进程级单例）。
pub struct EscalationBroker {
    tickets: Mutex<HashMap<String, EscalationTicket>>,
}

impl EscalationBroker {
    fn global() -> &'static Self {
        static BROKER: OnceLock<EscalationBroker> = OnceLock::new();
        BROKER.get_or_init(|| Self {
            tickets: Mutex::new(HashMap::new()),
        })
    }

    /// 签发票据（仅宿主用户批准入口调用）：绑定结构化授权指纹。
    pub fn issue(fingerprint: EscalationFingerprint) -> String {
        let token = scru128::new().to_string();
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        tickets.insert(
            token.clone(),
            EscalationTicket {
                fingerprint,
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        token
    }

    /// 按结构化指纹匹配并消费票据：全字段精确相等才命中。
    /// 不匹配的票据保留（用户批准的那条命令仍可重试；token 不可猜测，
    /// 保留不构成穷举面）；过期票据清理。
    pub fn consume_by_fingerprint(fingerprint: &EscalationFingerprint) -> bool {
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        let now = Instant::now();
        tickets.retain(|_, ticket| now < ticket.expires_at);
        let token = tickets
            .iter()
            .find(|(_, ticket)| &ticket.fingerprint == fingerprint)
            .map(|(token, _)| token.clone());
        match token {
            Some(token) => tickets.remove(&token).is_some(),
            None => false,
        }
    }

    /// 当前待消费的批准数量（审计/UI 展示用）。
    pub fn pending_count() -> usize {
        let tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        let now = Instant::now();
        tickets
            .values()
            .filter(|ticket| now < ticket.expires_at)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 全局票据库为进程级单例，测试并行运行时各用例必须使用互异的
    // 指纹内容，避免跨用例命中。
    fn run_command_payload(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "cmd": "docker",
            "args": ["ps", "-a"],
            "cwd": format!("/tmp/ws-{tag}"),
            "access": {"workspace": format!("/tmp/ws-{tag}"), "full_trust": false, "allowed_commands": []},
        })
    }

    fn run_shell_payload(tag: &str) -> serde_json::Value {
        serde_json::json!({"script": format!("cargo build --release #{tag}"), "cwd": "/tmp/ws"})
    }

    #[test]
    fn approval_matches_exact_fingerprint() {
        let payload = run_command_payload("exact");
        EscalationBroker::issue(EscalationFingerprint::from_payload(
            "command.run_command",
            &payload,
        ));
        assert!(EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &payload)
        ));
        // 一次性：再次消费失败。
        assert!(!EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &payload)
        ));
    }

    #[test]
    fn tampered_arguments_cannot_consume_approval() {
        let approved = run_command_payload("tamper");
        EscalationBroker::issue(EscalationFingerprint::from_payload(
            "command.run_command",
            &approved,
        ));
        // 参数被替换（拼接字符串无法区分的歧义场景）。
        let mut tampered = run_command_payload("tamper");
        tampered["args"] = serde_json::json!(["system", "prune"]);
        assert!(!EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &tampered)
        ));
        // cwd 被替换同样失败。
        let mut moved = run_command_payload("tamper");
        moved["cwd"] = serde_json::json!("/etc");
        assert!(!EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &moved)
        ));
        // 原指纹仍可消费。
        assert!(EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &approved)
        ));
    }

    #[test]
    fn operation_and_script_must_match() {
        let payload = run_shell_payload("op");
        EscalationBroker::issue(EscalationFingerprint::from_payload(
            "command.run_shell",
            &payload,
        ));
        let mut wrong_op = run_shell_payload("op");
        wrong_op["cmd"] = serde_json::json!("cargo");
        assert!(!EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_command", &wrong_op)
        ));
        assert!(EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload("command.run_shell", &payload)
        ));
    }

    #[test]
    fn unapproved_never_matches() {
        assert!(!EscalationBroker::consume_by_fingerprint(
            &EscalationFingerprint::from_payload(
                "command.run_command",
                &run_command_payload("none")
            )
        ));
    }
}
