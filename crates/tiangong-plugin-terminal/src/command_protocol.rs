use std::io::Write as _;
use std::sync::Arc;

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::manager::TerminalManager;
use crate::types::{contains_marker, PtyState, TerminalExecResponse, TerminalOutputEvent};

const DEFAULT_PROMPT_WAIT_SECS: u64 = 120;
/// start marker 出现后无 end marker 的兜底检测阈值（秒）
const INTERACTIVE_FALLBACK_SECS: u64 = 8;
/// 识别到等待输入提示后，输出稳定多久即可返回给 Agent 继续交互。
const INTERACTIVE_PROMPT_STABLE_MS: u64 = 700;

/// 发送命令到 PTY
fn send_to_pty(writer: &mut Box<dyn std::io::Write + Send>, input: &str) -> anyhow::Result<()> {
    writer.write_all(input.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// 把 `total_lines_pushed` 轨道值换算为环形缓冲区当前索引。
/// 环形缓冲区会从头部淘汰旧行，因此越早推入的行对应索引越小；
/// 若该时刻的行已被淘汰则返回 0（从缓冲区开头兜底）。
fn buf_idx_from_pushed(state: &crate::manager::TerminalState, pushed: usize) -> usize {
    let total = state.total_lines_pushed;
    if pushed >= total {
        0
    } else {
        let offset = total - pushed;
        state.output_buffer.len().saturating_sub(offset)
    }
}

/// 系统 PTY 启动失败或退出后的恢复：尝试重新拉起一次。
/// 成功时重置输出缓冲并启动输出读取线程，返回新的 PtyState；失败返回 None。
fn ensure_system_pty(manager: &Arc<TerminalManager>, app: &tauri::AppHandle) -> Option<PtyState> {
    let session_id = manager.session_id();
    let cwd = manager.cwd();
    let shell = manager.shell();
    tracing::info!(session_id = %session_id, "系统 PTY 不可用，尝试重新启动");
    match crate::manager::start_pty(&session_id, &cwd, &shell) {
        Ok(new_ps) => {
            manager.set_alive(true);
            {
                let mut state = manager.state.lock().unwrap();
                state.output_buffer.clear();
                state.last_read_line = 0;
                state.current_line.clear();
            }
            crate::output_processor::spawn_output_reader(
                new_ps.reader.clone(),
                manager.clone_state(),
                app.clone(),
                session_id.clone(),
            );
            tracing::info!(session_id = %session_id, "系统 PTY 已重新启动");
            Some(new_ps)
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = %e, "系统 PTY 重新启动失败");
            None
        }
    }
}

fn collect_command_output(
    manager: &Arc<TerminalManager>,
    start_marker: &str,
    end_marker: &str,
    cwd_marker: &str,
    rc_marker: &str,
    include_current_line: bool,
    // 未命中 start marker 时的兜底起点（环形缓冲区索引）。
    // 传 None 表示不限制（仅用于无法确定边界的场景）。
    fallback_start_idx: Option<usize>,
) -> (String, bool) {
    let state = manager.state.lock().unwrap();
    let mut in_range = false;
    let mut lines = Vec::new();
    for line in &state.output_buffer {
        if line.contains(start_marker) {
            in_range = true;
            continue;
        }
        if line.contains(end_marker) {
            break;
        }
        if in_range
            && !line.contains(cwd_marker)
            && !line.contains(rc_marker)
            && !contains_marker(line)
        {
            lines.push(line.clone());
        }
    }
    if !in_range {
        // 兜底：start marker 未命中（可能被环形缓冲区淘汰）。
        // 仅返回本次命令开始后新增的行，避免上次命令的残留输出混入。
        lines.clear();
        let start = fallback_start_idx.unwrap_or(0);
        for line in state.output_buffer.iter().skip(start) {
            if !contains_marker(line) && !line.contains(cwd_marker) && !line.contains(rc_marker) {
                lines.push(line.clone());
            }
        }
    }
    if include_current_line {
        let current_line = state.current_line.trim_end();
        if !current_line.trim().is_empty()
            && !contains_marker(current_line)
            && !current_line.contains(cwd_marker)
            && !current_line.contains(rc_marker)
            && lines.last().is_none_or(|line| line != current_line)
        {
            lines.push(current_line.to_string());
        }
    }
    let interrupted = lines
        .iter()
        .any(|l| l.contains("^C") || l.contains("SIGINT") || l.contains("Interrupt"));
    (lines.join("\n"), interrupted)
}

fn looks_like_interactive_prompt(text: &str) -> bool {
    let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let line = line.trim();
    let lower = line.to_lowercase();

    if matches!(line, ">>>" | "..." | ">" | "$" | "%" | "#") {
        return true;
    }
    if line.ends_with('$') || line.ends_with('%') || line.ends_with('#') {
        return true;
    }
    // 以 `?` 结尾的行：仅当它是短行（≤ 80 字符，排除正常程序输出中的疑问句）
    // 或包含明确的 yes/no 提示词时才判定为交互提示，避免把普通输出里的问句误判。
    if line.ends_with('?')
        && (line.chars().count() <= 80
            || lower.contains("yes/no")
            || lower.contains("(y/n")
            || lower.contains("[y/n"))
    {
        return true;
    }

    let prompt_fragments = [
        "password",
        "passphrase",
        "verification code",
        "one-time code",
        "otp",
        "username",
        "login:",
        "yes/no",
        "(y/n",
        "[y/n",
        "(yes/no",
        "[yes/no",
        "continue connecting",
        "are you sure",
        "do you want",
        "would you like",
        "proceed",
        "press enter",
        "press return",
        "hit enter",
        "select",
        "choice",
    ];
    if prompt_fragments
        .iter()
        .any(|fragment| lower.contains(fragment))
    {
        return true;
    }

    line.ends_with(':')
        && [
            "password",
            "passphrase",
            "username",
            "login",
            "token",
            "code",
            "input",
            "enter",
            "select",
            "choice",
            "name",
        ]
        .iter()
        .any(|fragment| lower.contains(fragment))
}

/// 非交互式命令执行：通过 marker 检测命令边界，捕获退出码和 cwd
pub(crate) async fn handle_exec(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    app: &tauri::AppHandle,
    command: &str,
    timeout_secs: Option<u64>,
    response_tx: oneshot::Sender<TerminalExecResponse>,
    activity: Option<&Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    let ps = match pty_state {
        Some(ps) => ps,
        None => {
            // 系统 PTY 启动失败或已退出的恢复路径：尝试重新拉起一次再执行，
            // 避免用户全程只能得到"终端会话不可用"。重试仍失败才返回错误。
            match ensure_system_pty(manager, app) {
                Some(new_ps) => {
                    *pty_state = Some(new_ps);
                    pty_state.as_mut().expect("PTY 刚写入，必然存在")
                }
                None => {
                    let _ = response_tx.send(TerminalExecResponse {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: "终端会话不可用".to_string(),
                        timed_out: false,
                        cwd_after: manager.cwd(),
                        interrupted_by_user: false,
                        interactive_mode: false,
                    });
                    return;
                }
            }
        }
    };

    // 如果当前处于 AgentInteractive 状态，先发送 Ctrl+C 清理残留前台进程
    // 场景：Agent 先用 interactive=true 启动 vi，vi 退出后 shell 回到 prompt，
    // 但状态仍为 AgentInteractive。后续 exec 需要确保 shell 回到干净状态。
    if let Some(tracker) = activity {
        if matches!(
            tracker.busy_state(),
            crate::collaboration::TerminalBusyState::AgentInteractive { .. }
        ) {
            if let Ok(mut writer) = ps.writer.lock() {
                let _ = writer.write_all(b"\x03");
                let _ = writer.flush();
            }
            // 等待 shell 处理 SIGINT 并回到 prompt
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    // 生成 start marker 和 end marker
    let marker_id = scru128::new();
    let command_id = marker_id.to_string();
    let start_marker = format!("__TIANGONG_START_{}__", marker_id);
    let end_marker = format!("__TIANGONG_END_{}__", marker_id);
    let cwd_marker = format!("__TIANGONG_CWD_{}__", marker_id);
    let rc_marker = format!("__TIANGONG_RC_{}__", marker_id);

    // 先设置协作状态为 AgentRunning，再发送命令
    if let Some(tracker) = activity {
        tracker.set_busy_state(crate::collaboration::TerminalBusyState::AgentRunning {
            command_id: command_id.clone(),
        });
    }

    // 向前端推送用户命令首行回显（shell 回显被 marker 过滤，需要手动补回）
    // 多行命令（如 heredoc）只回显首行，后续内容由 shell 回显自然展示
    {
        let first_line = command.lines().next().unwrap_or("");
        if !first_line.is_empty() {
            let echo = TerminalOutputEvent {
                session_id: manager.session_id(),
                text: format!("{}\n", first_line),
                is_echo: false,
            };
            let _ = app.emit("terminal:output", &echo);
        }
    }

    // 在发送命令前读取 total_lines_pushed，避免竞态条件：
    // 如果在发送后读取，shell 可能已经执行完 echo 命令并输出 marker，
    // 输出读取线程可能在读取 total_lines_pushed 之前就将其推入缓冲区，
    // 导致 marker 被算作"旧行"而永远不会被检测循环扫描到。
    // 这在 shell 热启动后（如交互式 vi 会话后）尤其容易发生。
    let mut last_seen_pushed: usize = {
        let state = manager.state.lock().unwrap();
        state.total_lines_pushed
    };

    // 兜底起点：未命中 start marker 时，仅收集本命令发出后新增的行，
    // 避免环形缓冲区中上次命令的残留输出混入。环形缓冲区会从头部淘汰，
    // 因此这里用 (当前累计行 - 命令起点累计行) 反推缓冲区索引。
    let fallback_start_pushed = last_seen_pushed;

    // 发送组合命令：start marker → 用户命令（禁用 pager）→ 捕获退出码 → pwd → end marker
    // PAGER=cat 禁用所有命令的分页器，GIT_PAGER=cat 兼容 git 特有变量
    // printf '\nmarker' 强制换行：即使命令输出不以 \n 结尾，marker 也独占新行
    // 换行分隔用户命令和 marker 捕获命令，确保 marker 行以 __TIANGONG_= 前缀开头
    // 这样多行命令（heredoc）的最后一行不会和 marker 命令混在一起泄漏
    let combined = format!(
        "__TIANGONG_= echo '{}'; PAGER=cat GIT_PAGER=cat {}\n__TIANGONG_= __rc=$?; printf '\\n{}'; pwd; echo '{}'$__rc; echo '{}'\n",
        start_marker, command, cwd_marker, rc_marker, end_marker
    );
    let send_result = {
        match ps.writer.lock() {
            Ok(mut writer) => send_to_pty(&mut writer, &combined),
            Err(e) => Err(anyhow::anyhow!("获取 writer 锁失败: {}", e)),
        }
    };
    if let Err(e) = send_result {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("发送命令到终端失败: {}", e),
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 等待 end marker 出现在输出中
    let timeout = timeout_secs.unwrap_or(DEFAULT_PROMPT_WAIT_SECS);
    let start_time = std::time::Instant::now();
    let timeout_dur = std::time::Duration::from_secs(timeout);
    let interactive_dur = std::time::Duration::from_secs(INTERACTIVE_FALLBACK_SECS);
    let interactive_prompt_stable_dur =
        std::time::Duration::from_millis(INTERACTIVE_PROMPT_STABLE_MS);
    let mut start_marker_time: Option<std::time::Instant> = None;
    let mut last_visible_snapshot = String::new();
    let mut last_visible_change = std::time::Instant::now();
    // 跨轮询持久状态：start marker 已出现
    let mut start_seen = false;
    let mut cwd_value = String::new();
    let mut rc_value: Option<i32> = None;

    loop {
        let result = {
            let state = manager.state.lock().unwrap();
            let pushed = state.total_lines_pushed;
            if pushed <= last_seen_pushed {
                None
            } else {
                // 计算自上次以来新增的行数，从缓冲区末尾取
                let new_count = pushed - last_seen_pushed;
                let buf_len = state.output_buffer.len();
                let start_idx = buf_len.saturating_sub(new_count);
                let mut found_end = false;
                for i in start_idx..buf_len {
                    if let Some(line) = state.output_buffer.get(i) {
                        if line.trim() == start_marker {
                            start_seen = true;
                        }
                        if line.trim() == end_marker {
                            found_end = true;
                        }
                        if line.starts_with(&cwd_marker) {
                            cwd_value = line[cwd_marker.len()..].trim().to_string();
                        }
                        if let Some(rest) = line.trim().strip_prefix(&rc_marker) {
                            rc_value = rest.trim().parse().ok();
                        }
                    }
                }
                if start_seen && start_marker_time.is_none() {
                    start_marker_time = Some(std::time::Instant::now());
                }
                last_seen_pushed = pushed;
                if found_end && start_seen {
                    Some(crate::types::CollectResult {
                        cwd: cwd_value.clone(),
                        exit_code: rc_value.unwrap_or(0),
                    })
                } else {
                    None
                }
            }
        };

        if let Some(collect) = result {
            // 收集 start marker 和 end marker 之间的输出
            let fallback_idx = {
                let state = manager.state.lock().unwrap();
                buf_idx_from_pushed(&state, fallback_start_pushed)
            };
            let (stdout_text, interrupted) = collect_command_output(
                manager,
                &start_marker,
                &end_marker,
                &cwd_marker,
                &rc_marker,
                false,
                Some(fallback_idx),
            );

            let cwd = if collect.cwd.is_empty() {
                manager.cwd()
            } else {
                // 更新 cwd
                let mut state = manager.state.lock().unwrap();
                state.cwd = collect.cwd.clone();
                collect.cwd
            };

            let tracker_interrupted = activity.map(|t| t.take_user_intervened()).unwrap_or(false);
            if let Some(tracker) = activity {
                tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
            }
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: collect.exit_code,
                stdout: stdout_text,
                stderr: String::new(),
                timed_out: false,
                cwd_after: cwd,
                interrupted_by_user: interrupted || tracker_interrupted,
                interactive_mode: false,
            });
            return;
        }

        // 兜底交互检测：start marker 已出现但 end marker 未出现，且终端已经有可见输出。
        if let Some(smt) = start_marker_time {
            let fallback_idx = {
                let state = manager.state.lock().unwrap();
                buf_idx_from_pushed(&state, fallback_start_pushed)
            };
            let (stdout_text, interrupted) = collect_command_output(
                manager,
                &start_marker,
                &end_marker,
                &cwd_marker,
                &rc_marker,
                true,
                Some(fallback_idx),
            );
            if stdout_text != last_visible_snapshot {
                last_visible_snapshot = stdout_text.clone();
                last_visible_change = std::time::Instant::now();
            }
            let has_visible_output = !stdout_text.trim().is_empty();
            let prompt_ready = has_visible_output
                && looks_like_interactive_prompt(&stdout_text)
                && last_visible_change.elapsed() >= interactive_prompt_stable_dur;
            let fallback_ready = has_visible_output && smt.elapsed() >= interactive_dur;

            if prompt_ready || fallback_ready {
                let reason = if prompt_ready {
                    "命令正在等待交互输入"
                } else {
                    "命令未返回结束标记，已返回当前终端显示内容"
                };

                let tracker_interrupted =
                    activity.map(|t| t.take_user_intervened()).unwrap_or(false);
                if let Some(tracker) = activity {
                    tracker.set_busy_state(
                        crate::collaboration::TerminalBusyState::AgentInteractive {
                            command_id: command_id.clone(),
                        },
                    );
                }
                let _ = response_tx.send(TerminalExecResponse {
                    exit_code: 0,
                    stdout: stdout_text,
                    stderr: reason.to_string(),
                    timed_out: false,
                    cwd_after: manager.cwd(),
                    interrupted_by_user: interrupted || tracker_interrupted,
                    interactive_mode: true,
                });
                return;
            }
        }

        if start_time.elapsed() >= timeout_dur {
            // 超时，先发送 Ctrl+C 中断前台进程，等待 shell 回到 prompt
            {
                if let Ok(mut writer) = ps.writer.lock() {
                    // 发送 Ctrl+C 中断前台进程
                    let _ = writer.write_all(b"\x03");
                    let _ = writer.flush();
                }
            }
            // 等待短暂时间让 shell 回到 prompt
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // 再发一次 Ctrl+C 并发送回车确保 shell 回到干净状态
            // 注意用 `\r`（CR）而非 `\n`：PTY 线路规程只识别 CR 作为回车提交，
            // 发 LF 在 zsh ZLE / 交互程序中无效，可能导致 shell 卡在未提交状态
            {
                if let Ok(mut writer) = ps.writer.lock() {
                    let _ = writer.write_all(b"\x03\r");
                    let _ = writer.flush();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            // 收集已捕获的输出
            let fallback_idx = {
                let state = manager.state.lock().unwrap();
                buf_idx_from_pushed(&state, fallback_start_pushed)
            };
            let (stdout_text, interrupted) = collect_command_output(
                manager,
                &start_marker,
                &end_marker,
                &cwd_marker,
                &rc_marker,
                true,
                Some(fallback_idx),
            );

            let tracker_interrupted = activity.map(|t| t.take_user_intervened()).unwrap_or(false);
            if let Some(tracker) = activity {
                tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
            }
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: -1,
                stdout: stdout_text,
                stderr: "命令执行超时".to_string(),
                timed_out: true,
                cwd_after: manager.cwd(),
                interrupted_by_user: interrupted || tracker_interrupted,
                interactive_mode: false,
            });
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 交互式命令执行：不使用 marker，直接发送命令，等待初始输出后返回
pub(crate) async fn handle_exec_interactive(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    command: &str,
    wait_secs: u64,
    response_tx: oneshot::Sender<TerminalExecResponse>,
    activity: Option<&Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    let ps = match pty_state {
        Some(ps) => ps,
        None => {
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: "终端会话不可用".to_string(),
                timed_out: false,
                cwd_after: manager.cwd(),
                interrupted_by_user: false,
                interactive_mode: false,
            });
            return;
        }
    };

    let record_start = {
        let state = manager.state.lock().unwrap();
        state.output_buffer.len()
    };

    // 先设置协作状态为 AgentInteractive
    let command_id = scru128::new().to_string();
    if let Some(tracker) = activity {
        tracker.set_busy_state(crate::collaboration::TerminalBusyState::AgentInteractive {
            command_id: command_id.clone(),
        });
    }

    // 清理残留进程 + 发送命令
    // 注意：用 `\r`（CR）而非 `\n`（LF）作为回车键。zsh 的 ZLE、vim、less 等
    // TUI 程序在 raw 模式下只识别 CR 作为"提交本行"，发 LF 会导致命令停留在
    // 输入行不执行——历史上观察到 `vi hello.txt\n` 后 shell 不执行的现象。
    // （终端上看到的 `vvi hello.txt` 是 zsh autosuggestion 的视觉提示，与
    // 实际行缓冲无关，无需为此加 sleep。）
    let send_result = {
        match ps.writer.lock() {
            Ok(mut writer) => {
                let _ = writer.write_all(b"\x03");
                let _ = writer.flush();
                let _ = writer.write_all(b"\x15");
                let _ = writer.flush();
                let cmd = format!("{}\r", command);
                send_to_pty(&mut writer, &cmd)
            }
            Err(e) => Err(anyhow::anyhow!("获取 writer 锁失败: {}", e)),
        }
    };
    if let Err(e) = send_result {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("发送命令到终端失败: {}", e),
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 等待初始输出
    tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;

    // 收集本次命令后的输出，过滤掉内部 marker（start/end/cwd/rc），
    // 避免上一次 exec 残留的 marker 行泄漏到交互输出中
    let stdout_text = {
        let state = manager.state.lock().unwrap();
        state
            .output_buffer
            .iter()
            .skip(record_start)
            .filter(|line| !contains_marker(line))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    };

    let _ = response_tx.send(TerminalExecResponse {
        exit_code: 0,
        stdout: stdout_text,
        stderr: String::new(),
        timed_out: false,
        cwd_after: manager.cwd(),
        interrupted_by_user: false,
        interactive_mode: true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager_with_lines(lines: &[&str]) -> Arc<TerminalManager> {
        let manager = Arc::new(TerminalManager::new("test".to_string(), "/tmp".to_string()));
        {
            let mut state = manager.state.lock().unwrap();
            for line in lines {
                crate::manager::push_output(&mut state, line.to_string());
            }
        }
        manager
    }

    #[test]
    fn collect_extracts_range_between_markers() {
        let manager = make_manager_with_lines(&[
            "old output",
            "__TIANGONG_START_abc__",
            "hello",
            "world",
            "__TIANGONG_CWD_abc__/tmp",
            "__TIANGONG_RC_abc__0",
            "__TIANGONG_END_abc__",
        ]);
        let (out, interrupted) = collect_command_output(
            &manager,
            "__TIANGONG_START_abc__",
            "__TIANGONG_END_abc__",
            "__TIANGONG_CWD_abc__",
            "__TIANGONG_RC_abc__",
            false,
            None,
        );
        assert_eq!(out, "hello\nworld");
        assert!(!interrupted);
    }

    #[test]
    fn collect_detects_interrupt_in_range() {
        let manager = make_manager_with_lines(&[
            "__TIANGONG_START_abc__",
            "running",
            "^C",
            "__TIANGONG_END_abc__",
        ]);
        let (_, interrupted) = collect_command_output(
            &manager,
            "__TIANGONG_START_abc__",
            "__TIANGONG_END_abc__",
            "__TIANGONG_CWD_abc__",
            "__TIANGONG_RC_abc__",
            false,
            None,
        );
        assert!(interrupted);
    }

    #[test]
    fn collect_fallback_respects_start_idx() {
        // 缓冲区：[旧行, 新行1, 新行2]，start marker 缺失。
        // fallback_start_idx=1 时只应返回"新行1/新行2"，不含旧行。
        let manager = make_manager_with_lines(&["old residual", "new line 1", "new line 2"]);
        let (out, _) = collect_command_output(
            &manager,
            "__TIANGONG_START_missing__",
            "__TIANGONG_END_missing__",
            "__TIANGONG_CWD_missing__",
            "__TIANGONG_RC_missing__",
            false,
            Some(1),
        );
        assert_eq!(out, "new line 1\nnew line 2");
    }

    #[test]
    fn collect_fallback_without_idx_returns_all() {
        let manager = make_manager_with_lines(&["old", "new"]);
        let (out, _) = collect_command_output(
            &manager,
            "__TIANGONG_START_missing__",
            "__TIANGONG_END_missing__",
            "__TIANGONG_CWD_missing__",
            "__TIANGONG_RC_missing__",
            false,
            None,
        );
        assert_eq!(out, "old\nnew");
    }

    #[test]
    fn interactive_prompt_matches_shell_prompts() {
        assert!(looks_like_interactive_prompt("user@host:~$"));
        assert!(looks_like_interactive_prompt(">>> "));
        assert!(looks_like_interactive_prompt("Password:"));
        assert!(looks_like_interactive_prompt("Continue? [y/n] "));
    }

    #[test]
    fn interactive_prompt_short_question_matches() {
        // 短问句（≤ 80 字符）视为交互提示
        assert!(looks_like_interactive_prompt("Proceed with installation?"));
    }

    #[test]
    fn interactive_prompt_long_question_does_not_match() {
        // 长疑问句（> 80 字符）不应误判为交互提示
        let long = "这是一段非常长的程序输出文字".repeat(10) + "?";
        assert!(!looks_like_interactive_prompt(&long));
    }

    #[test]
    fn interactive_prompt_plain_output_does_not_match() {
        assert!(!looks_like_interactive_prompt(
            "Build completed successfully"
        ));
        assert!(!looks_like_interactive_prompt(""));
    }
}
