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
    /// 授权的操作类型（`command.run_command` / `command.run_shell`）。
    operation: String,
    /// 授权的完整命令文本（cmd + args 拼接 / 完整脚本文本），精确匹配。
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
    /// 绑定操作类型与完整命令文本：用户批准了什么就只能执行什么。
    pub fn issue(operation: impl Into<String>, command: impl Into<String>) -> String {
        let token = scru128::new().to_string();
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        tickets.insert(
            token.clone(),
            EscalationTicket {
                operation: operation.into(),
                command: command.into(),
                expires_at: Instant::now() + TICKET_TTL,
            },
        );
        token
    }

    /// 验证并消费票据：token 有效、未过期、命令匹配（前缀匹配程序名）。
    /// 命令不匹配时回填票据（保留用户已批准命令的重试机会；token 不可
    /// 猜测，回填不构成穷举面）。成功或过期即消费。
    pub fn verify_and_consume(token: &str, operation: &str, command: &str) -> bool {
        let mut tickets = Self::global().tickets.lock().expect("升级票据库锁已损坏");
        let Some(ticket) = tickets.remove(token) else {
            return false;
        };
        if Instant::now() >= ticket.expires_at {
            return false; // 过期：已移除即消费。
        }
        let matched = ticket.operation == operation && ticket.command == command;
        if !matched {
            tickets.insert(token.to_string(), ticket);
            return false;
        }
        true
    }
}

/// 宿主命令路由的升级声明核验（透明执行封套）：
/// 返回 `(改写后的 payload, 是否已获票据授权)`——有效票据剥离 token 保留
/// 审批依据并返回 true；无效票据剥离声明返回 false（回退沙箱执行）。
pub fn verify_and_strip_escalation(
    operation: &str,
    mut payload: serde_json::Value,
) -> (serde_json::Value, bool) {
    let is_exec = matches!(operation, "command.run_command" | "command.run_shell");
    if !is_exec {
        return (payload, false);
    }
    let Some(escalated) = payload
        .as_object_mut()
        .and_then(|map| map.remove("escalated"))
    else {
        return (payload, false);
    };
    let token = escalated
        .get("token")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // 完整命令文本：run_command 用 cmd + args 拼接；run_shell 用完整脚本；
    // trust_command 用命令程序名。
    let command = if operation == "command.run_shell" {
        payload
            .get("script")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        let cmd = payload
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let args = payload
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        if args.is_empty() {
            cmd
        } else {
            format!("{cmd} {args}")
        }
    };
    if EscalationBroker::verify_and_consume(token, operation, &command) {
        tracing::warn!(operation, command = %command, "升级票据核验通过（一次性消费）");
        // 剥离 token：sidecar 只收到审批依据（数据通道），无票据可转发。
        let mut escalated = escalated;
        if let Some(map) = escalated.as_object_mut() {
            map.remove("token");
        }
        if let Some(map) = payload.as_object_mut() {
            map.insert("escalated".to_string(), escalated);
        }
        (payload, true)
    } else {
        tracing::warn!(
            operation,
            command = %command,
            "升级票据无效，escalated 声明被剥离（回退沙箱执行）"
        );
        (payload, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ticket_is_one_shot_and_exact_command_bound() {
        let token = EscalationBroker::issue("command.run_command", "docker ps -a");
        assert!(EscalationBroker::verify_and_consume(
            &token,
            "command.run_command",
            "docker ps -a"
        ));
        // 一次性：第二次消费失败。
        assert!(!EscalationBroker::verify_and_consume(
            &token,
            "command.run_command",
            "docker ps -a"
        ));
        // 完整命令不匹配：批准"docker ps"不能换成"docker rm"。
        let token2 = EscalationBroker::issue("command.run_command", "docker ps");
        assert!(!EscalationBroker::verify_and_consume(
            &token2,
            "command.run_command",
            "docker system prune"
        ));
        // 不匹配回填后，原命令仍可用。
        assert!(EscalationBroker::verify_and_consume(
            &token2,
            "command.run_command",
            "docker ps"
        ));
        // 操作类型不匹配。
        let token3 = EscalationBroker::issue("command.run_command", "cargo build");
        assert!(!EscalationBroker::verify_and_consume(
            &token3,
            "command.run_shell",
            "cargo build"
        ));
    }

    #[test]
    fn invalid_ticket_strips_escalated_for_exec() {
        let payload = json!({
            "cmd": "mkfs.ext4",
            "escalated": {"approval_note": "伪造批准", "token": "bogus"}
        });
        let (sanitized, granted) = verify_and_strip_escalation("command.run_command", payload);
        assert!(sanitized.get("escalated").is_none());
        assert_eq!(sanitized["cmd"], "mkfs.ext4");
        assert!(!granted);
    }

    #[test]
    fn valid_ticket_keeps_escalated_without_token() {
        let token = EscalationBroker::issue("command.run_command", "docker system prune");
        let payload = json!({
            "cmd": "docker system prune",
            "escalated": {"approval_note": "用户批准", "token": token}
        });
        let (sanitized, granted) = verify_and_strip_escalation("command.run_command", payload);
        assert!(granted);
        let escalated = sanitized.get("escalated").unwrap();
        assert_eq!(escalated["approval_note"], "用户批准");
        assert!(escalated.get("token").is_none());
    }

    #[test]
    fn run_shell_ticket_binds_full_script() {
        let token = EscalationBroker::issue("command.run_shell", "cargo build --release");
        let payload = json!({
            "script": "cargo build --release",
            "escalated": {"approval_note": "用户批准", "token": token}
        });
        let sanitized = enforce_escalation_ticket("command", "command.run_shell", payload).unwrap();
        assert!(sanitized.get("escalated").is_some());

        // 脚本被篡改：票据剥离，回退沙箱。
        let token2 = EscalationBroker::issue("command.run_shell", "cargo build --release");
        let payload = json!({
            "script": "cargo build && rm -rf /tmp/x",
            "escalated": {"approval_note": "用户批准", "token": token2}
        });
        let sanitized = enforce_escalation_ticket("command", "command.run_shell", payload).unwrap();
        assert!(sanitized.get("escalated").is_none());
    }

    #[test]
    fn run_command_ticket_binds_args() {
        let token = EscalationBroker::issue("command.run_command", "docker ps -a");
        let payload = json!({
            "cmd": "docker",
            "args": ["ps", "-a"],
            "escalated": {"approval_note": "用户批准", "token": token}
        });
        let (sanitized, granted) = verify_and_strip_escalation("command.run_command", payload);
        assert!(sanitized.get("escalated").is_some());
    }
}
