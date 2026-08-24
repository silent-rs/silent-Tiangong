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

struct EscalationTicket {
    operation: String,
    command: String,
    expires_at: Instant,
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

    /// 签发票据（仅宿主用户批准入口调用）：绑定操作类型与完整命令文本。
    pub fn issue(operation: impl Into<String>, command: impl Into<String>) -> String {
        let operation = operation.into();
        let command = command.into();
        let token = scru128::new().to_string();
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        tickets.insert(
            token.clone(),
            EscalationTicket {
                operation,
                command,
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        token
    }

    /// 按完整命令文本匹配并消费票据：操作与命令文本精确相等才命中。
    /// 不匹配的票据保留（用户批准的那条命令仍可重试；token 不可猜测，
    /// 保留不构成穷举面）；过期票据清理。
    pub fn consume_by_command(operation: &str, command: &str) -> bool {
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        let now = Instant::now();
        tickets.retain(|_, ticket| now < ticket.expires_at);
        let token = tickets
            .iter()
            .find(|(_, ticket)| ticket.operation == operation && ticket.command == command)
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

    #[test]
    fn approval_matches_exact_command_text_only() {
        EscalationBroker::issue("command.run_command", "docker ps -a");
        // 完整文本精确匹配（含参数）。
        assert!(EscalationBroker::consume_by_command(
            "command.run_command",
            "docker ps -a"
        ));
        // 一次性：再次消费失败。
        assert!(!EscalationBroker::consume_by_command(
            "command.run_command",
            "docker ps -a"
        ));
    }

    #[test]
    fn tampered_command_cannot_consume_approval() {
        // 批准"docker ps"后换成"docker system prune"无法消费。
        EscalationBroker::issue("command.run_command", "docker ps");
        assert!(!EscalationBroker::consume_by_command(
            "command.run_command",
            "docker system prune"
        ));
        // 原命令仍可消费（不匹配不消费他人批准）。
        assert!(EscalationBroker::consume_by_command(
            "command.run_command",
            "docker ps"
        ));
    }

    #[test]
    fn operation_must_match() {
        EscalationBroker::issue("command.run_shell", "cargo build --release");
        assert!(!EscalationBroker::consume_by_command(
            "command.run_command",
            "cargo build --release"
        ));
        assert!(EscalationBroker::consume_by_command(
            "command.run_shell",
            "cargo build --release"
        ));
    }

    #[test]
    fn unapproved_command_never_matches() {
        assert!(!EscalationBroker::consume_by_command(
            "command.run_command",
            "rm -rf /"
        ));
    }
}
