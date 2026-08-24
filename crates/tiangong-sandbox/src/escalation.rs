//! 升级审批票据（RFC 0017 S4）：宿主签发、一次性消费、短时效。
//!
//! 安全边界：`escalated` 声明的信任锚是宿主票据库，不是请求方声明——
//! 能构造 sidecar 请求的一方（Agent/插件）无法自行"已获批准"。
//! 票据仅由宿主入口签发（桌面 UI 的用户显式批准动作，
//! [`crate::escalation::issue`]），Agent 经桥接层不可触达签发路径。
//!
//! 验证发生在宿主转发层（`invoke_sidecar`）：失败即剥离声明回退沙箱执行，
//! `trust_command` 等高权操作直接拒绝。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 票据有效期：用户批准后 Agent 需在窗口内使用。
const TICKET_TTL: Duration = Duration::from_secs(300);

struct EscalationTicket {
    /// 授权的命令程序名（如 `docker`）。
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

    /// 签发一次性票据（仅宿主用户批准入口调用）。
    pub fn issue(command: impl Into<String>) -> String {
        let token = scru128::new().to_string();
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        tickets.insert(
            token.clone(),
            EscalationTicket {
                command: command.into(),
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        token
    }

    /// 验证并消费票据：token 有效、未过期、命令匹配（前缀匹配程序名）。
    /// 命令不匹配时回填票据（保留用户已批准命令的重试机会；token 不可
    /// 猜测，回填不构成穷举面）。成功或过期即消费。
    pub fn verify_and_consume(token: &str, command: &str) -> bool {
        let program = command
            .rsplit('/')
            .next()
            .unwrap_or(command)
            .split_whitespace()
            .next()
            .unwrap_or_default();
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        let Some(ticket) = tickets.remove(token) else {
            return false;
        };
        if Instant::now() >= ticket.expires_at {
            return false; // 过期：已移除即消费。
        }
        let matched =
            ticket.command == program || program.starts_with(&format!("{} ", ticket.command));
        if !matched {
            tickets.insert(token.to_string(), ticket);
            return false;
        }
        true
    }
}

/// 宿主转发层的升级声明核验：
/// - `run_command` / `run_shell`：票据无效时剥离 `escalated`（回退沙箱执行）；
/// - `trust_command`：票据无效时返回 `Err`（高权操作直接拒绝）。
pub fn enforce_escalation_ticket(
    plugin_id: &str,
    operation: &str,
    mut payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if plugin_id != "command" {
        return Ok(payload);
    }
    let is_exec = matches!(operation, "command.run_command" | "command.run_shell");
    let is_trust = operation == "command.trust_command";
    if !is_exec && !is_trust {
        return Ok(payload);
    }
    let Some(escalated) = payload
        .as_object_mut()
        .and_then(|map| map.remove("escalated"))
    else {
        // 未携带声明：run 走沙箱；trust 无票据直接拒绝。
        if is_trust {
            return Err("trust_command 需要宿主签发的升级审批票据".to_string());
        }
        return Ok(payload);
    };
    let token = escalated
        .get("token")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let command = if is_trust {
        payload
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        payload
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if EscalationBroker::verify_and_consume(token, &command) {
        tracing::warn!(plugin_id, operation, command = %command, "升级票据核验通过（一次性消费）");
        // 剥离 token，sidecar 只保留审批依据做审计。
        let mut escalated = escalated;
        if let Some(map) = escalated.as_object_mut() {
            map.remove("token");
        }
        if let Some(map) = payload.as_object_mut() {
            map.insert("escalated".to_string(), escalated);
        }
        Ok(payload)
    } else if is_trust {
        Err("升级审批票据无效或已过期，拒绝 trust_command".to_string())
    } else {
        tracing::warn!(
            plugin_id,
            operation,
            command = %command,
            "升级票据无效，escalated 声明被剥离（回退沙箱执行）"
        );
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ticket_is_one_shot_and_command_bound() {
        let token = EscalationBroker::issue("docker");
        assert!(EscalationBroker::verify_and_consume(&token, "docker ps -a"));
        // 一次性：第二次消费失败。
        assert!(!EscalationBroker::verify_and_consume(&token, "docker ps"));
        // 命令不匹配。
        let token2 = EscalationBroker::issue("cargo");
        assert!(!EscalationBroker::verify_and_consume(&token2, "rm -rf /"));
        assert!(EscalationBroker::verify_and_consume(&token2, "cargo build"));
    }

    #[test]
    fn invalid_ticket_strips_escalated_for_exec() {
        let payload = json!({
            "cmd": "mkfs.ext4",
            "escalated": {"approval_note": "伪造批准", "token": "bogus"}
        });
        let sanitized =
            enforce_escalation_ticket("command", "command.run_command", payload).unwrap();
        assert!(sanitized.get("escalated").is_none());
        assert_eq!(sanitized["cmd"], "mkfs.ext4");
    }

    #[test]
    fn valid_ticket_keeps_escalated_without_token() {
        let token = EscalationBroker::issue("docker");
        let payload = json!({
            "cmd": "docker system prune",
            "escalated": {"approval_note": "用户批准", "token": token}
        });
        let sanitized =
            enforce_escalation_ticket("command", "command.run_command", payload).unwrap();
        let escalated = sanitized.get("escalated").unwrap();
        assert_eq!(escalated["approval_note"], "用户批准");
        assert!(escalated.get("token").is_none());
    }

    #[test]
    fn trust_command_rejected_without_valid_ticket() {
        let payload = json!({"command": "docker", "approval_note": "伪造"});
        assert!(enforce_escalation_ticket("command", "command.trust_command", payload).is_err());

        let token = EscalationBroker::issue("docker");
        let payload = json!({
            "command": "docker",
            "approval_note": "用户批准",
            "escalated": {"approval_note": "用户批准", "token": token}
        });
        let sanitized =
            enforce_escalation_ticket("command", "command.trust_command", payload).unwrap();
        assert!(sanitized.get("escalated").is_some());
    }
}
