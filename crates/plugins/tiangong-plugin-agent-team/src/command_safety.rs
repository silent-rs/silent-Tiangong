//! 子 Agent 命令工具的锁生命周期约束。
//!
//! 文件锁只能覆盖当前工具 Future。会在工具返回后继续运行的后台、脱离式或交互式
//! 进程会绕过锁，因此子 Agent 必须在启动前被拒绝；主 Agent 不受此策略影响。

use std::path::Path;

use tiangong_core::model::ToolCall;

use crate::constants::MAX_SUB_AGENT_COMMAND_TIMEOUT_SECS;

pub(crate) fn guard_sub_agent_command(call: &ToolCall, workspace: &Path) -> Result<(), String> {
    match call.name.as_str() {
        "terminal_send" => {
            return Err(
                "Sub Agent 不得操作会在工具返回后继续运行的交互终端；请改用前台命令".to_string(),
            );
        }
        "run_shell"
            if call
                .arguments
                .get("interactive")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false) =>
        {
            return Err("Sub Agent 不得启动交互式命令；交互进程会超出文件锁生命周期".to_string());
        }
        "run_command" | "run_shell" => {}
        _ => return Ok(()),
    }

    ensure_bounded_command_timeout(call)?;

    if let Some(raw_cwd) = call
        .arguments
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
    {
        let effective_cwd = tiangong_toolkit::resolve_effective_cwd_with(Some(raw_cwd), workspace)
            .map_err(|error| format!("Sub Agent 命令工作目录无效：{error}"))?;
        ensure_inside_workspace(&effective_cwd, workspace)?;
    }

    let command_text = command_text(call);
    if contains_detached_execution(&command_text) {
        return Err(
            "Sub Agent 不得启动后台、脱离式或会持久驻留的命令；请使用前台命令并等待其结束后再释放文件锁"
                .to_string(),
        );
    }
    Ok(())
}

fn ensure_bounded_command_timeout(call: &ToolCall) -> Result<(), String> {
    let timeout = call
        .arguments
        .get("timeout")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.trim().parse().ok()))
        })
        .ok_or_else(|| {
            format!(
                "Sub Agent 前台命令必须显式设置 1..={MAX_SUB_AGENT_COMMAND_TIMEOUT_SECS} 秒的 timeout，确保执行不超出文件锁租期"
            )
        })?;
    if !(1..=MAX_SUB_AGENT_COMMAND_TIMEOUT_SECS).contains(&timeout) {
        return Err(format!(
            "Sub Agent 前台命令 timeout 必须在 1..={MAX_SUB_AGENT_COMMAND_TIMEOUT_SECS} 秒之间，确保执行不超出文件锁租期"
        ));
    }
    Ok(())
}

fn ensure_inside_workspace(path: &Path, workspace: &Path) -> Result<(), String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("解析工作区失败：{error}"))?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("解析命令工作目录失败：{error}"))?;
    if path.starts_with(&workspace) {
        Ok(())
    } else {
        Err(format!(
            "Sub Agent 命令只能在当前工作区内运行：{}",
            path.display()
        ))
    }
}

fn command_text(call: &ToolCall) -> String {
    let mut parts = Vec::new();
    if let Some(command) = call
        .arguments
        .get("cmd")
        .and_then(serde_json::Value::as_str)
    {
        parts.push(command.to_string());
    }
    if let Some(script) = call
        .arguments
        .get("script")
        .and_then(serde_json::Value::as_str)
    {
        parts.push(script.to_string());
    }
    if let Some(arguments) = call
        .arguments
        .get("args")
        .and_then(serde_json::Value::as_array)
    {
        parts.extend(
            arguments
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string),
        );
    }
    parts.join(" ")
}

fn contains_detached_execution(command: &str) -> bool {
    if contains_unquoted_single_ampersand(command) {
        return true;
    }

    let lowered = command.to_ascii_lowercase();
    let normalized = lowered
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    tokens.iter().any(|token| {
        matches!(
            *token,
            "nohup"
                | "disown"
                | "setsid"
                | "screen"
                | "tmux"
                | "systemd-run"
                | "start-process"
                | "launchctl"
        )
    }) || tokens.windows(2).any(|window| window == ["start", "b"])
}

fn contains_unquoted_single_ampersand(command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if !in_single_quote => escaped = true,
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '&' if !in_single_quote && !in_double_quote => {
                let previous = index.checked_sub(1).and_then(|i| chars.get(i));
                let next = chars.get(index + 1);
                if previous != Some(&'&') && next != Some(&'&') {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn foreground_commands_inside_workspace_are_allowed() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("nested");
        std::fs::create_dir(&nested).unwrap();

        guard_sub_agent_command(
            &call(
                "run_shell",
                serde_json::json!({
                    "script": "cargo check && cargo test",
                    "cwd": nested,
                    "timeout": 120
                }),
            ),
            workspace.path(),
        )
        .unwrap();
    }

    #[test]
    fn detached_and_interactive_commands_are_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        for command in [
            call(
                "run_shell",
                serde_json::json!({ "script": "server &", "timeout": 120 }),
            ),
            call(
                "run_shell",
                serde_json::json!({ "script": "nohup server", "timeout": 120 }),
            ),
            call(
                "run_command",
                serde_json::json!({ "cmd": "tmux new-session -d server", "timeout": 120 }),
            ),
            call(
                "run_shell",
                serde_json::json!({ "script": "vim", "interactive": true }),
            ),
            call("terminal_send", serde_json::json!({ "input": "q" })),
        ] {
            assert!(guard_sub_agent_command(&command, workspace.path()).is_err());
        }
    }

    #[test]
    fn quoted_ampersand_and_foreground_and_are_not_misclassified() {
        let workspace = tempfile::tempdir().unwrap();
        for script in ["echo 'a & b'", "cargo check && cargo test"] {
            guard_sub_agent_command(
                &call(
                    "run_shell",
                    serde_json::json!({ "script": script, "timeout": 120 }),
                ),
                workspace.path(),
            )
            .unwrap();
        }
    }

    #[test]
    fn command_cwd_outside_workspace_is_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let result = guard_sub_agent_command(
            &call(
                "run_command",
                serde_json::json!({
                    "cmd": "cargo check",
                    "cwd": outside.path(),
                    "timeout": 120
                }),
            ),
            workspace.path(),
        );

        assert!(result.unwrap_err().contains("命令工作目录"));
    }

    #[test]
    fn command_timeout_must_fit_inside_the_lock_lease() {
        let workspace = tempfile::tempdir().unwrap();
        for timeout in [None, Some(0), Some(241)] {
            let mut arguments = serde_json::json!({ "cmd": "cargo check" });
            if let Some(timeout) = timeout {
                arguments["timeout"] = serde_json::json!(timeout);
            }
            assert!(
                guard_sub_agent_command(&call("run_command", arguments), workspace.path())
                    .unwrap_err()
                    .contains("timeout")
            );
        }

        guard_sub_agent_command(
            &call(
                "run_command",
                serde_json::json!({ "cmd": "cargo check", "timeout": 240 }),
            ),
            workspace.path(),
        )
        .unwrap();
    }
}
