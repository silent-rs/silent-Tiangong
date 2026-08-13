use std::io::Write as _;
use std::sync::Arc;

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::manager::TerminalManager;
use crate::types::{PtyState, TerminalExecResponse, TerminalOutputEvent};

const COMMAND_START_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const SHELL_READY_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const SHELL_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// 发送命令到 PTY
fn send_to_pty(writer: &mut Box<dyn std::io::Write + Send>, input: &str) -> anyhow::Result<()> {
    writer.write_all(input.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// 解码 Agent 在 terminal_send input 中使用的转义序列为真实控制字节。
///
/// Agent 按工具规格用字面转义序列表示特殊键（如 `\x1b:wq\r`），但 JSON 字符串里
/// `` 是 4 个字面字符（反斜杠+x+1+b）。写入 PTY 前需解码为真实控制字节（0x1b），
/// 否则 vi 等程序会把它当普通文本插入而非 ESC 指令。
///
/// 转义在终端执行侧（写入 PTY 前）处理，而非 handler 发送侧——这样 handler 只负责
/// 取参数原样传递，转义细节内聚在终端层。
///
/// 使用 descape 库解码（支持 \e/\x1b=ESC、\r=CR、\n=LF、\xHH=任意字节、
/// \uXXXX=unicode 等完整 C 风格转义）。解码失败（含非法转义）时回退到原文，
/// 保证不丢数据——最坏情况是字面字符写入 PTY（与修复前行为一致）。
fn decode_terminal_escapes(input: &str) -> String {
    use descape::UnescapeExt;
    match input.to_unescaped() {
        Ok(cow) => cow.into_owned(),
        Err(e) => {
            tracing::warn!(
                index = e.index,
                input_len = input.len(),
                "terminal_send input 含非法转义序列，原样写入 PTY"
            );
            input.to_string()
        }
    }
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
fn ensure_system_pty<R: tauri::Runtime>(
    manager: &Arc<TerminalManager>,
    app: &tauri::AppHandle<R>,
) -> Option<PtyState> {
    if manager.is_closed() {
        return None;
    }
    let session_id = manager.session_id();
    let cwd = manager.cwd();
    let shell = manager.shell();
    let pty_env = manager
        .pty_env
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    tracing::info!(session_id = %session_id, "系统 PTY 不可用，尝试重新启动");
    match crate::manager::start_pty(&session_id, &cwd, &shell, &pty_env) {
        Ok(new_ps) => {
            {
                let mut state = manager.state.lock().unwrap();
                state.output_buffer.clear();
                state.last_read_line = 0;
                state.current_line.clear();
            }
            let generation = manager.activate_pty(new_ps.writer.clone());
            crate::output_processor::spawn_output_reader(
                new_ps.reader.clone(),
                manager.clone_state(),
                app.clone(),
                session_id.clone(),
                manager.logger.clone(),
                generation,
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
        if line_matches_marker(line, start_marker) {
            in_range = true;
            continue;
        }
        if line_matches_marker(line, end_marker) {
            break;
        }
        // 用精确匹配过滤 marker 行（cwd/rc marker 行），排除 shell 回显的
        // wrapper 命令文本（它 contains 但不 == marker）。
        let is_cwd = extract_marker_value(line, cwd_marker).is_some();
        let is_rc = extract_marker_value(line, rc_marker).is_some();
        // 只过滤本次命令的实际 marker（含唯一 ID），不用固定前缀——
        // 避免用户输出恰好包含 __TIANGONG_START_ 前缀被静默删除。
        let is_start = line_matches_marker(line, start_marker);
        let is_end = line_matches_marker(line, end_marker);
        if in_range && !is_cwd && !is_rc && !is_start && !is_end {
            lines.push(line.clone());
        }
    }
    if !in_range {
        // 兜底：start marker 未命中（可能被环形缓冲区淘汰）。
        // 仅返回本次命令开始后新增的行，避免上次命令的残留输出混入。
        lines.clear();
        let start = fallback_start_idx.unwrap_or(0);
        for line in state.output_buffer.iter().skip(start) {
            let is_cwd = extract_marker_value(line, cwd_marker).is_some();
            let is_rc = extract_marker_value(line, rc_marker).is_some();
            let is_start = line_matches_marker(line, start_marker);
            let is_end = line_matches_marker(line, end_marker);
            if !is_cwd && !is_rc && !is_start && !is_end {
                lines.push(line.clone());
            }
        }
    }
    if include_current_line {
        let current_line = state.current_line.trim_end();
        let is_cwd = extract_marker_value(current_line, cwd_marker).is_some();
        let is_rc = extract_marker_value(current_line, rc_marker).is_some();
        let is_start = line_matches_marker(current_line, start_marker);
        let is_end = line_matches_marker(current_line, end_marker);
        if !current_line.trim().is_empty()
            && !is_cwd
            && !is_rc
            && !is_start
            && !is_end
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

/// 剥离 CSI/OSC 等 ANSI 转义序列，返回纯文本。
///
/// marker 匹配前先 strip ANSI，这样即使 TTY-aware 命令在 marker 行前后残留
/// 转义片断（如 `\x1b[0m`），也能用精确 `==` 判定——避免 `contains` 误匹配
/// shell 回显的 wrapper 命令文本（回显行包含所有 marker 字符串）。
fn strip_ansi(input: &str) -> String {
    strip_ansi_escapes::strip_str(input)
}

/// 判定一行是否匹配某个 marker（先 strip ANSI 再精确比较 trim 后的行）。
///
/// 不用 `contains`：shell 会回显 wrapper 命令文本，该行包含所有 marker 字符串，
/// `contains` 会误匹配回显导致命令尚未执行就判定完成。
fn line_matches_marker(line: &str, marker: &str) -> bool {
    strip_ansi(line).trim() == marker
}

/// 从 marker 行提取 marker 之后的值（先 strip ANSI）。
/// 用于 cwd_marker / rc_marker 后面跟着的路径 / 退出码。
fn extract_marker_value(line: &str, marker: &str) -> Option<String> {
    let clean = strip_ansi(line);
    let trimmed = clean.trim();
    let rest = trimmed.strip_prefix(marker)?;
    let value = rest.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[derive(Debug)]
enum ShellPreparationError {
    Cancelled,
    Unavailable(String),
}

fn shell_ready_probe(marker: &str) -> String {
    format!("echo '{marker}'\r")
}

async fn wait_for_shell_condition(
    manager: &TerminalManager,
    deadline: std::time::Instant,
    cancellation: Option<&crate::types::TerminalExecCancellation>,
    response_tx: &oneshot::Sender<TerminalExecResponse>,
    condition: impl Fn(&crate::manager::TerminalState) -> bool,
) -> Result<(), ShellPreparationError> {
    loop {
        if response_tx.is_closed() || cancellation.is_some_and(|value| value.is_requested()) {
            return Err(ShellPreparationError::Cancelled);
        }
        if !manager.is_alive() {
            return Err(ShellPreparationError::Unavailable(
                "终端进程已退出".to_string(),
            ));
        }
        let condition_met = {
            let state = manager
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            condition(&state)
        };
        if condition_met {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(ShellPreparationError::Unavailable(
                "等待 Shell 就绪超时".to_string(),
            ));
        }
        tokio::time::sleep(SHELL_READY_POLL_INTERVAL).await;
    }
}

async fn confirm_shell_ready(
    manager: &Arc<TerminalManager>,
    interrupt: bool,
    cancellation: Option<&crate::types::TerminalExecCancellation>,
    response_tx: &oneshot::Sender<TerminalExecResponse>,
) -> Result<(), ShellPreparationError> {
    if response_tx.is_closed() || cancellation.is_some_and(|value| value.is_requested()) {
        return Err(ShellPreparationError::Cancelled);
    }

    let deadline = std::time::Instant::now() + SHELL_READY_CONFIRM_TIMEOUT;
    wait_for_shell_condition(manager, deadline, cancellation, response_tx, |state| {
        !state.output_buffer.is_empty() || !state.current_line.is_empty()
    })
    .await?;

    if interrupt {
        let baseline_lines = manager.total_lines_pushed();
        manager
            .write_input(b"\x03")
            .map_err(ShellPreparationError::Unavailable)?;
        // Linux Shell 会在异步处理 SIGINT 时再次清空输入队列。等它输出 Ctrl+C
        // 对应的完整行后再发探针，避免探针的开头仍落在清理窗口中。
        wait_for_shell_condition(manager, deadline, cancellation, response_tx, |state| {
            state.total_lines_pushed > baseline_lines
        })
        .await?;
    }

    let marker = format!("{}{}__", crate::types::MARKER_READY, scru128::new());
    manager
        .write_input(shell_ready_probe(&marker).as_bytes())
        .map_err(ShellPreparationError::Unavailable)?;
    wait_for_shell_condition(manager, deadline, cancellation, response_tx, |state| {
        state
            .output_buffer
            .iter()
            .any(|line| line_matches_marker(line, &marker))
    })
    .await
}

fn discard_current_pty(manager: &TerminalManager, pty_state: &mut Option<PtyState>) {
    manager.deactivate_pty();
    if let Some(ps) = pty_state.take() {
        crate::manager::shutdown_pty(ps);
    }
}

async fn prepare_shell_for_agent_command<R: tauri::Runtime>(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    app: &tauri::AppHandle<R>,
    cancellation: Option<&crate::types::TerminalExecCancellation>,
    response_tx: &oneshot::Sender<TerminalExecResponse>,
) -> Result<(), ShellPreparationError> {
    match confirm_shell_ready(manager, true, cancellation, response_tx).await {
        Ok(()) => return Ok(()),
        Err(ShellPreparationError::Cancelled) => {
            discard_current_pty(manager, pty_state);
            return Err(ShellPreparationError::Cancelled);
        }
        Err(ShellPreparationError::Unavailable(error)) => {
            tracing::warn!(
                session_id = %manager.session_id(),
                %error,
                "终端清理后未就绪，重建 PTY"
            );
            discard_current_pty(manager, pty_state);
        }
    }

    if manager.is_closed() {
        return Err(ShellPreparationError::Unavailable("终端已关闭".to_string()));
    }
    *pty_state = ensure_system_pty(manager, app);
    if pty_state.is_none() {
        return Err(ShellPreparationError::Unavailable(
            "终端会话不可用".to_string(),
        ));
    }

    match confirm_shell_ready(manager, false, cancellation, response_tx).await {
        Ok(()) => Ok(()),
        Err(error) => {
            discard_current_pty(manager, pty_state);
            Err(error)
        }
    }
}

fn prepare_non_interactive_command(
    start_marker: &str,
    command: &str,
    cwd_marker: &str,
    rc_marker: &str,
    end_marker: &str,
) -> anyhow::Result<(Option<tempfile::NamedTempFile>, String)> {
    // Windows/PowerShell 直接将包装脚本粘贴到 PTY。不使用临时文件——#355 引入的
    // 临时文件方式在 Windows 上因文件无扩展名，PowerShell dot-source 会回退到
    // ShellExecute 触发"你要如何打开此文件"弹窗，导致命令卡死超时。PowerShell
    // 不存在 zsh 的多行拆分问题，直接粘贴是安全的。
    if cfg!(target_os = "windows") {
        return Ok(prepare_powershell_command(
            start_marker,
            command,
            cwd_marker,
            rc_marker,
            end_marker,
        ));
    }

    let command = if command_requires_isolation(command) {
        format!("(\n{command}\n)")
    } else {
        command.to_string()
    };
    let mut script = tempfile::Builder::new()
        .prefix(&format!("{start_marker}_SCRIPT_"))
        .tempfile()?;
    write!(
        script,
        "__TIANGONG_= echo '{}'; __tiangong_had_PAGER=${{PAGER+x}}; __tiangong_old_PAGER=${{PAGER-}}; __tiangong_had_GIT_PAGER=${{GIT_PAGER+x}}; __tiangong_old_GIT_PAGER=${{GIT_PAGER-}}; __tiangong_had_GH_PAGER=${{GH_PAGER+x}}; __tiangong_old_GH_PAGER=${{GH_PAGER-}}; __tiangong_had_LESS=${{LESS+x}}; __tiangong_old_LESS=${{LESS-}}; export PAGER=cat GIT_PAGER=cat GH_PAGER=cat LESS=FRX\n{}\n__TIANGONG_= __rc=$?; if [ -n \"$__tiangong_had_PAGER\" ]; then PAGER=\"$__tiangong_old_PAGER\"; export PAGER; else unset PAGER; fi; if [ -n \"$__tiangong_had_GIT_PAGER\" ]; then GIT_PAGER=\"$__tiangong_old_GIT_PAGER\"; export GIT_PAGER; else unset GIT_PAGER; fi; if [ -n \"$__tiangong_had_GH_PAGER\" ]; then GH_PAGER=\"$__tiangong_old_GH_PAGER\"; export GH_PAGER; else unset GH_PAGER; fi; if [ -n \"$__tiangong_had_LESS\" ]; then LESS=\"$__tiangong_old_LESS\"; export LESS; else unset LESS; fi; printf '\\n{}'; pwd; echo '{}'$__rc; echo '{}'\n",
        start_marker, command, cwd_marker, rc_marker, end_marker
    )?;
    script.flush()?;
    let path = script.path().to_string_lossy();
    let invocation = format!(". {}\r", crate::util::shell_quote(path.as_ref()));
    Ok((Some(script), invocation))
}

/// Windows PowerShell 包装脚本：直接粘贴到 PTY 执行，不经过临时文件。
///
/// 输出的 marker 格式与 POSIX 版本完全一致（`line_matches_marker` 精确匹配），
/// 退出码捕获同时处理外部程序（`$LASTEXITCODE`）和 cmdlet（`$?`）两种情况。
fn prepare_powershell_command(
    start_marker: &str,
    command: &str,
    cwd_marker: &str,
    rc_marker: &str,
    end_marker: &str,
) -> (Option<tempfile::NamedTempFile>, String) {
    let mut ps = String::new();
    // 保存并临时设置 PAGER 等环境变量，避免 git diff/log 等命令落入交互式分页器
    ps.push_str("$__t_old_PAGER=$env:PAGER; $env:PAGER='cat'; ");
    ps.push_str("$__t_old_GIT_PAGER=$env:GIT_PAGER; $env:GIT_PAGER='cat'; ");
    ps.push_str("$__t_old_GH_PAGER=$env:GH_PAGER; $env:GH_PAGER='cat'; ");
    ps.push_str("$__t_old_LESS=$env:LESS; $env:LESS='FRX'\r\n");
    // 输出 start marker
    ps.push_str(&format!("Write-Output '{start_marker}'\r\n"));
    // 用户命令
    ps.push_str(command);
    ps.push_str("\r\n");
    // 捕获退出码：外部程序用 $LASTEXITCODE，cmdlet 用 $?
    ps.push_str("$__t_rc=if($?){if($null -ne $LASTEXITCODE){$LASTEXITCODE}else{0}}else{1}\r\n");
    // 恢复环境变量
    ps.push_str(
        "$env:PAGER=$__t_old_PAGER; $env:GIT_PAGER=$__t_old_GIT_PAGER; \
         $env:GH_PAGER=$__t_old_GH_PAGER; $env:LESS=$__t_old_LESS\r\n",
    );
    // 输出 cwd/rc/end marker（格式与 POSIX 版本一致：marker 前缀直接拼接值）
    ps.push_str("Write-Output ''\r\n");
    ps.push_str(&format!(
        "Write-Output \"{}$((Get-Location).Path)\"\r\n",
        cwd_marker
    ));
    ps.push_str(&format!("Write-Output \"{}$__t_rc\"\r\n", rc_marker));
    ps.push_str(&format!("Write-Output '{end_marker}'\r\n"));
    (None, ps)
}

/// 会退出或改变长期 shell 错误处理行为的控制语句放入子 shell 执行。
/// 普通命令仍在当前 shell 执行，以保留 cwd、环境变量和函数等会话状态。
fn command_requires_isolation(command: &str) -> bool {
    if cfg!(target_os = "windows") {
        return false;
    }

    command
        .lines()
        .flat_map(|line| line.split(';'))
        .map(str::trim)
        .filter(|statement| !statement.is_empty() && !statement.starts_with('#'))
        .any(|statement| {
            let mut words = statement.split_whitespace();
            let Some(first) = words.next() else {
                return false;
            };
            match first {
                "exit" | "logout" | "return" | "exec" | "trap" => true,
                "set" => match words.next() {
                    Some(option) if option.starts_with('-') && option[1..].contains('e') => true,
                    Some("-o") => words.next().is_some_and(|name| name == "errexit"),
                    _ => false,
                },
                "setopt" => words.any(|option| {
                    option.eq_ignore_ascii_case("errexit")
                        || option.eq_ignore_ascii_case("err_exit")
                }),
                _ => false,
            }
        })
}

/// 取得 PTY 当前前台进程组。交互 shell 开启 job control 后，运行中的用户命令
/// 位于该进程组；取消升级时必须终止整个组，不能只终止 shell 或只发送 Ctrl+C。
fn command_boundary_seen(manager: &TerminalManager, start_marker: &str, end_marker: &str) -> bool {
    let state = manager
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut start_seen = false;
    for line in &state.output_buffer {
        if line_matches_marker(line, start_marker) {
            start_seen = true;
        }
        if start_seen && line_matches_marker(line, end_marker) {
            return true;
        }
    }
    false
}

/// Ctrl+C 只是协作式取消，命令可以捕获或忽略 SIGINT。宽限期结束后若本次命令
/// 的 end marker 仍未出现，升级终止 PTY 前台进程组。返回前必须取得一个可证明
/// 的安全终态：命令边界已闭合，或不可忽略的终止信号已成功投递。
async fn stop_cancelled_command(
    manager: &Arc<TerminalManager>,
    ps: &mut crate::types::PtyState,
    start_marker: &str,
    end_marker: &str,
) -> bool {
    #[cfg(unix)]
    let process_group = crate::manager::foreground_process_group(ps);
    #[cfg(unix)]
    let shell_process_id = ps.child.process_id().map(|pid| pid as libc::pid_t);

    if let Ok(mut writer) = ps.writer.lock() {
        let _ = writer.write_all(b"\x03");
        let _ = writer.flush();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if command_boundary_seen(manager, start_marker, end_marker) {
        return false;
    }

    if let Ok(mut writer) = ps.writer.lock() {
        // CR 让已经回到 ZLE、但尚未输出 marker 的 shell 提交当前空行。
        let _ = writer.write_all(b"\x03\r");
        let _ = writer.flush();
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    if command_boundary_seen(manager, start_marker, end_marker) {
        return false;
    }

    #[cfg(unix)]
    if let Some(process_group) = process_group {
        if crate::manager::force_stop_process_group(process_group).is_ok() {
            return shell_process_id == Some(process_group);
        }
    }

    // 非 Unix 平台没有可移植的前台进程组接口；Unix 上查询/终止进程组异常时，
    // 直接终止 PTY shell 是最后一道保险。只有不可忽略信号成功投递或 shell 已退出
    // 才返回，避免 portable-pty 在 Unix 上只发 SIGHUP、被 shell 忽略后永久等待。
    #[cfg(unix)]
    if let Some(shell_process_id) = shell_process_id {
        if crate::manager::force_stop_process(shell_process_id).is_ok() {
            return true;
        }
    }

    loop {
        if let Ok(Some(_)) = ps.child.try_wait() {
            return true;
        }
        #[cfg(unix)]
        let kill_result = ps
            .child
            .process_id()
            .map(|pid| crate::manager::force_stop_process(pid as libc::pid_t));
        #[cfg(not(unix))]
        let kill_result = Some(ps.child.kill());
        if kill_result.is_some_and(|result| result.is_ok()) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// 非交互式命令执行：通过 marker 检测命令边界，捕获退出码和 cwd
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_exec<R: tauri::Runtime>(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    app: &tauri::AppHandle<R>,
    command: &str,
    timeout_secs: Option<u64>,
    response_tx: oneshot::Sender<TerminalExecResponse>,
    cancellation: Arc<crate::types::TerminalExecCancellation>,
    _completion: crate::types::TerminalExecCompletion,
    activity: Option<&Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    if cancellation.is_requested() || response_tx.is_closed() {
        return;
    }
    if manager.is_closed() {
        if let Some(ps) = pty_state.take() {
            crate::manager::shutdown_pty(ps);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: "终端已关闭，命令未执行".to_string(),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }
    if !manager.is_alive() {
        if let Some(ps) = pty_state.take() {
            crate::manager::shutdown_pty(ps);
        }
    }
    if pty_state.is_none() {
        // 系统 PTY 启动失败或已退出的恢复路径：尝试重新拉起一次再执行，
        // 避免用户全程只能得到"终端会话不可用"。重试仍失败才返回错误。
        *pty_state = ensure_system_pty(manager, app);
    }
    if pty_state.is_none() {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: "终端会话不可用".to_string(),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 生成 start marker 和 end marker
    let marker_id = scru128::new();
    let command_id = marker_id.to_string();
    let start_marker = format!("__TIANGONG_START_{}__", marker_id);
    let end_marker = format!("__TIANGONG_END_{}__", marker_id);
    let cwd_marker = format!("__TIANGONG_CWD_{}__", marker_id);
    let rc_marker = format!("__TIANGONG_RC_{}__", marker_id);

    // 先占用终端，再清理完整输入缓冲并确认 Shell 已重新接管。Ctrl+U 只能清理
    // 当前编辑行，无法取消未闭合的多行命令；就绪标记确认前不得发送真实命令。
    if let Some(tracker) = activity {
        tracker.set_busy_state(crate::collaboration::TerminalBusyState::AgentRunning {
            command_id: command_id.clone(),
        });
    }
    if let Err(error) = prepare_shell_for_agent_command(
        manager,
        pty_state,
        app,
        Some(cancellation.as_ref()),
        &response_tx,
    )
    .await
    {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let cancelled = matches!(&error, ShellPreparationError::Cancelled);
        let stderr = match error {
            ShellPreparationError::Cancelled => "命令执行已取消".to_string(),
            ShellPreparationError::Unavailable(error) => {
                format!("终端清理后无法恢复，命令未发送: {error}")
            }
        };
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr,
            terminal_error: !cancelled,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: cancelled,
            interactive_mode: false,
        });
        return;
    }

    // 包装脚本从独立文件读取，PTY 只接收一条短 source 命令。这样前台程序无法
    // 从 stdin 吞掉收尾标记，zsh 也不会把多行内部包装拆成残缺语句执行。
    let (_command_script, combined) = match prepare_non_interactive_command(
        &start_marker,
        command,
        &cwd_marker,
        &rc_marker,
        &end_marker,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(tracker) = activity {
                tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
            }
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("准备终端命令失败: {error}"),
                terminal_error: true,
                timed_out: false,
                cwd_after: manager.cwd(),
                interrupted_by_user: false,
                interactive_mode: false,
            });
            return;
        }
    };

    // 内部 source 命令会被 marker 过滤，向前端和持久日志补回用户实际提交的完整命令。
    if !command.is_empty() {
        let mut display_text = command
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "\r\n");
        if !display_text.ends_with("\r\n") {
            display_text.push_str("\r\n");
        }
        if let Some(logger) = &manager.logger {
            logger.append(&display_text);
        }
        let echo = TerminalOutputEvent {
            session_id: manager.session_id(),
            text: display_text,
            is_echo: true,
        };
        let _ = app.emit("terminal:output", &echo);
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

    // 发送临时脚本调用：start marker → 用户命令（禁用 pager）→ 捕获退出码 → pwd → end marker
    // 临时导出 PAGER/GIT_PAGER 指向 cat，避免 git diff/log 等命令落入 less；
    // 命令结束后恢复原环境，避免污染用户终端。
    // printf '\nmarker' 强制换行：即使命令输出不以 \n 结尾，marker 也独占新行
    // 换行分隔用户命令和 marker 捕获命令，确保 marker 行以 __TIANGONG_= 前缀开头
    // 这样多行命令（heredoc）的最后一行不会和 marker 命令混在一起泄漏
    let send_result = manager
        .write_input(combined.as_bytes())
        .map_err(anyhow::Error::msg);
    if let Err(e) = send_result {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("发送命令到终端失败: {}", e),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        if let Some(ps) = pty_state.take() {
            crate::manager::shutdown_pty(ps);
        }
        return;
    }

    // 等待 end marker 出现在输出中
    let start_time = std::time::Instant::now();
    let timeout_dur = timeout_secs.map(std::time::Duration::from_secs);
    // 跨轮询持久状态：start marker 已出现
    let mut start_seen = false;
    // 极高输出量可能在第一次轮询前就淘汰 start marker；这种情况不能误判为未启动。
    let mut start_boundary_may_have_scrolled = false;
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
                if new_count > buf_len {
                    start_boundary_may_have_scrolled = true;
                }
                let start_idx = buf_len.saturating_sub(new_count);
                let mut found_end = false;
                for i in start_idx..buf_len {
                    if let Some(line) = state.output_buffer.get(i) {
                        // 精确匹配（strip ANSI 后 == marker），不用 contains：
                        // shell 会回显 wrapper 命令文本，该行包含所有 marker 字符串，
                        // contains 会误匹配回显导致命令尚未执行就判定完成。
                        if line_matches_marker(line, start_marker.as_str()) {
                            start_seen = true;
                        }
                        if line_matches_marker(line, end_marker.as_str()) {
                            found_end = true;
                        }
                        if let Some(value) = extract_marker_value(line, cwd_marker.as_str()) {
                            cwd_value = value;
                        }
                        if let Some(value) = extract_marker_value(line, rc_marker.as_str()) {
                            if let Ok(rc) = value.parse::<i32>() {
                                rc_value = Some(rc);
                            }
                        }
                    }
                }
                last_seen_pushed = pushed;
                // 完成判定（按可靠性递减）：
                // 1. end + start marker 都命中 → 正常完成
                // 2. rc_marker 已收到（退出码已知）→ 命令一定已结束，
                //    end_marker 只是紧随其后的收尾 echo，可能被 ANSI 污染漏判。
                //    此时不应再等满超时——这正是 #237 的核心：命令成功执行、数据
                //    已采集，却因边界检测失败被误判超时，触发 recovery 封禁工具。
                let completed = (found_end && start_seen) || rc_value.is_some();
                if completed {
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
                terminal_error: false,
                timed_out: false,
                cwd_after: cwd,
                interrupted_by_user: interrupted || tracker_interrupted,
                interactive_mode: false,
            });
            return;
        }

        if !manager.is_alive() {
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
                stderr: if manager.is_closed() {
                    "终端已关闭，命令已中止".to_string()
                } else {
                    "终端进程已退出，下一次执行将自动重建".to_string()
                },
                terminal_error: true,
                timed_out: false,
                cwd_after: manager.cwd(),
                interrupted_by_user: interrupted || tracker_interrupted,
                interactive_mode: false,
            });
            if let Some(ps) = pty_state.take() {
                crate::manager::shutdown_pty(ps);
            }
            return;
        }

        let cancelled = cancellation.is_requested() || response_tx.is_closed();
        let timed_out = timeout_dur.is_some_and(|timeout| start_time.elapsed() >= timeout);
        if cancelled || timed_out {
            // 完成栅栏只会在命令边界闭合，或前台进程组收到不可忽略终止后释放。
            // 因此即使用户命令捕获/忽略 SIGINT，也不能越过 Agent Team 文件锁。
            let shell_stopped = match pty_state.as_mut() {
                Some(ps) => stop_cancelled_command(manager, ps, &start_marker, &end_marker).await,
                None => true,
            };

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
                stderr: if cancelled {
                    "命令执行已取消".to_string()
                } else {
                    "命令执行超时".to_string()
                },
                terminal_error: false,
                timed_out: !cancelled,
                cwd_after: manager.cwd(),
                interrupted_by_user: cancelled || interrupted || tracker_interrupted,
                interactive_mode: false,
            });
            if shell_stopped {
                // 强制终止 shell 后丢弃失效 PTY；下一条命令会走统一恢复入口重建。
                manager.deactivate_pty();
                if let Some(ps) = pty_state.take() {
                    crate::manager::shutdown_pty(ps);
                }
            }
            return;
        }

        if !start_seen
            && !start_boundary_may_have_scrolled
            && start_time.elapsed() >= COMMAND_START_CONFIRM_TIMEOUT
        {
            tracing::warn!(
                session_id = %manager.session_id(),
                "终端命令未收到启动标记，停止当前 PTY"
            );
            if let Some(ps) = pty_state.as_mut() {
                let _ = stop_cancelled_command(manager, ps, &start_marker, &end_marker).await;
            }

            let fallback_idx = {
                let state = manager.state.lock().unwrap();
                buf_idx_from_pushed(&state, fallback_start_pushed)
            };
            let (stdout_text, _) = collect_command_output(
                manager,
                &start_marker,
                &end_marker,
                &cwd_marker,
                &rc_marker,
                true,
                Some(fallback_idx),
            );
            if let Some(tracker) = activity {
                tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
            }
            manager.deactivate_pty();
            if let Some(ps) = pty_state.take() {
                crate::manager::shutdown_pty(ps);
            }
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: -1,
                stdout: stdout_text,
                stderr: "命令未能在终端中启动，已停止该终端".to_string(),
                terminal_error: true,
                timed_out: false,
                cwd_after: manager.cwd(),
                interrupted_by_user: false,
                interactive_mode: false,
            });
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 交互式命令执行：不使用 marker 协议，直接以 CR 提交命令，
/// 轮询等待终端"第一次变化"后返回完整可见内容。
///
/// 适用场景：需要持续键盘交互的程序。
/// 与 `handle_exec` 的关键差异：
/// - 不发 marker（交互程序前台时 marker 会污染屏幕甚至被读走）
/// - 协作状态进入 `AgentInteractive` 而非 `AgentRunning`
/// - 不轮询 marker，而是检测 output_buffer 相比基线是否出现增长（程序开始渲染）
/// - 退出码恒为 0、不更新 cwd（交互程序不会回 end/cwd marker）
///
/// 返回内容：终端当前可见文本（ANSI 已处理）。交互程序进入任何状态
///（全屏界面 / 确认提示 / 等待输入）都会在第一次渲染时被捕获，
/// Agent 直接阅读返回内容即可判断当前状态，无需后端识别提示类型。
/// 等待终端屏幕发生变化（双信号 + 稳定窗口），用于交互程序首屏/响应检测。
///
/// 双信号策略：
/// - `screen_updates`（前端 xterm 快照）：最准确的屏幕内容，优先等此信号
/// - `output_pushed`（后端 buffer）：可靠的后端信号，前端未回传时退到此信号
///
/// 稳定窗口：信号首次触发后，继续轮询 `stable_window`（300ms）确认无进一步变化，
/// 认为屏幕已渲染稳定。快照信号用 1x 窗口，output 信号用 2x 窗口（精度较低等更久）。
/// 超过 `wait_secs` 仍无信号则放弃等待（返回，调用方用当前快照兜底）。
async fn wait_for_screen_change(
    manager: &Arc<TerminalManager>,
    baseline_pushed: usize,
    baseline_updates: u64,
    wait_secs: u64,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs.max(1));
    let stable_window = std::time::Duration::from_millis(300);
    let poll_interval = std::time::Duration::from_millis(50);
    let mut output_changed_at: Option<std::time::Instant> = None;
    let mut screen_changed_at: Option<std::time::Instant> = None;

    loop {
        if !manager.is_alive() {
            break;
        }
        let now_pushed = manager.total_lines_pushed();
        let now_updates = manager.screen_updates();
        if output_changed_at.is_none() && now_pushed > baseline_pushed {
            output_changed_at = Some(std::time::Instant::now());
        }
        if screen_changed_at.is_none() && now_updates > baseline_updates {
            screen_changed_at = Some(std::time::Instant::now());
        }
        if let Some(change_time) = screen_changed_at {
            if change_time.elapsed() >= stable_window {
                break;
            }
        }
        if screen_changed_at.is_none() {
            if let Some(change_time) = output_changed_at {
                if change_time.elapsed() >= stable_window * 2 {
                    break;
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

pub(crate) async fn handle_exec_interactive<R: tauri::Runtime>(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    app: &tauri::AppHandle<R>,
    command: &str,
    wait_secs: u64,
    response_tx: oneshot::Sender<TerminalExecResponse>,
    activity: Option<&Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    if manager.is_closed() {
        if let Some(ps) = pty_state.take() {
            crate::manager::shutdown_pty(ps);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: "终端已关闭，命令未执行".to_string(),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }
    if !manager.is_alive() {
        if let Some(ps) = pty_state.take() {
            crate::manager::shutdown_pty(ps);
        }
    }
    if pty_state.is_none() {
        *pty_state = ensure_system_pty(manager, app);
    }
    if pty_state.is_none() {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: "终端会话不可用".to_string(),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }
    if let Err(error) =
        prepare_shell_for_agent_command(manager, pty_state, app, None, &response_tx).await
    {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let cancelled = matches!(&error, ShellPreparationError::Cancelled);
        let stderr = match error {
            ShellPreparationError::Cancelled => "交互命令执行已取消".to_string(),
            ShellPreparationError::Unavailable(error) => {
                format!("终端清理后无法恢复，交互命令未发送: {error}")
            }
        };
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr,
            terminal_error: !cancelled,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: cancelled,
            interactive_mode: false,
        });
        return;
    }
    let Some(ps) = pty_state.as_mut() else {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: "终端会话不可用".to_string(),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    };

    // 发送命令前记录双信号基线：
    // - output_pushed：程序输出到达后端 buffer 的可靠信号（立即可知）
    // - screen_updates：前端 xterm 渲染并回传快照的信号（最终屏幕内容来源）
    let baseline_pushed = manager.total_lines_pushed();
    let baseline_updates = manager.screen_updates();

    // Shell 已通过就绪标记确认，发送真实命令。
    // 注意：用 `\r`（CR）而非 `\n`（LF）作为回车键。zsh 的 ZLE、vim、less 等
    // TUI 程序在 raw 模式下只识别 CR 作为"提交本行"，发 LF 会导致命令停留在
    // 输入行不执行。
    let send_result = {
        match ps.writer.lock() {
            Ok(mut writer) => {
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
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 命令已成功发送，设置协作状态为 AgentInteractive：该 session 的 tracker
    // 会被标记为"Agent 正在交互执行"，用户在面板输入会被记为干预。
    let command_id = scru128::new().to_string();
    if let Some(tracker) = activity {
        tracker.set_busy_state(crate::collaboration::TerminalBusyState::AgentInteractive {
            command_id: command_id.clone(),
        });
    }

    // 双信号等待：交互程序首屏渲染（见 wait_for_screen_change）
    wait_for_screen_change(manager, baseline_pushed, baseline_updates, wait_secs).await;

    if !manager.is_alive() {
        if let Some(tracker) = activity {
            tracker.set_busy_state(crate::collaboration::TerminalBusyState::Idle);
        }
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: manager.recent_output(80),
            stderr: if manager.is_closed() {
                "终端已关闭，交互命令已中止".to_string()
            } else {
                "终端进程已退出".to_string()
            },
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 返回终端当前可见内容：优先前端 xterm 回传的屏幕快照（与用户看到的画面一致，
    // 全屏 TUI 和交互提示都能完整呈现），若前端尚未回传（面板未挂载等）
    // 则回退到后端 output_buffer 的可见内容（recent_output 兜底，保证即时可见性）。
    let stdout_text = manager
        .screen_snapshot()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| manager.recent_output(80));

    let _ = response_tx.send(TerminalExecResponse {
        exit_code: 0,
        stdout: stdout_text,
        stderr: String::new(),
        terminal_error: false,
        timed_out: false,
        cwd_after: manager.cwd(),
        interrupted_by_user: false,
        interactive_mode: true,
    });
}

/// 向已进入交互态的终端发送输入（按键/文本），等待屏幕变化后返回新快照。
///
/// 这是 Agent 持续操作交互程序的核心：每发一次按键，等待屏幕渲染稳定后返回
/// 当前可见内容。Agent 据此观察程序反应，形成"输入→观察→输入"闭环。
/// 不发 marker、不设置协作状态（交互程序启动时已是 AgentInteractive 态）。
/// 复用 `handle_exec_interactive` 的"等待 screen_updates 变化 + 稳定窗口"轮询逻辑。
pub(crate) async fn handle_send_interactive<R: tauri::Runtime>(
    manager: &Arc<TerminalManager>,
    pty_state: &mut Option<PtyState>,
    _app: &tauri::AppHandle<R>,
    input: &str,
    wait_secs: u64,
    response_tx: oneshot::Sender<TerminalExecResponse>,
    // _activity 不使用：send_interactive 发的是对已运行交互程序的按键，
    // 此时协作状态在 exec_interactive 启动时已设为 AgentInteractive，无需重复设置。
    _activity: Option<&Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    if manager.is_closed() || !manager.is_alive() {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: if manager.is_closed() {
                "终端已关闭，无法发送交互输入".to_string()
            } else {
                "终端会话不可用".to_string()
            },
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }
    let ps = match pty_state {
        Some(ps) => ps,
        None => {
            let _ = response_tx.send(TerminalExecResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: "终端会话不可用".to_string(),
                terminal_error: true,
                timed_out: false,
                cwd_after: manager.cwd(),
                interrupted_by_user: false,
                interactive_mode: false,
            });
            return;
        }
    };

    // 发送前记录双信号基线：
    // - output_pushed：程序响应输出到达后端 buffer 的可靠信号（立即可知）
    // - screen_updates：前端 xterm 渲染并回传快照的信号（最终屏幕内容来源）
    let baseline_pushed = manager.total_lines_pushed();
    let baseline_updates = manager.screen_updates();

    // 把输入写入 PTY。输入由 Agent 给出（vi 按键、REPL 命令等）。
    // Agent 用转义序列表示特殊键（\x1b=ESC、\r=CR），在终端执行侧解码为真实控制字节。
    // 不做 LF 转 CR：send_interactive 发的是对前台程序的原始输入，由 Agent 控制按键。
    let decoded_input = decode_terminal_escapes(input);
    let send_result = {
        match ps.writer.lock() {
            Ok(mut writer) => send_to_pty(&mut writer, &decoded_input),
            Err(e) => Err(anyhow::anyhow!("获取 writer 锁失败: {}", e)),
        }
    };
    if let Err(e) = send_result {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("发送输入到终端失败: {}", e),
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    // 双信号等待：交互程序对输入的响应（见 wait_for_screen_change）
    wait_for_screen_change(manager, baseline_pushed, baseline_updates, wait_secs).await;

    if !manager.is_alive() {
        let _ = response_tx.send(TerminalExecResponse {
            exit_code: -1,
            stdout: manager.recent_output(80),
            stderr: if manager.is_closed() {
                "终端已关闭，交互输入已中止".to_string()
            } else {
                "终端进程已退出".to_string()
            },
            terminal_error: true,
            timed_out: false,
            cwd_after: manager.cwd(),
            interrupted_by_user: false,
            interactive_mode: false,
        });
        return;
    }

    let stdout_text = manager
        .screen_snapshot()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| manager.recent_output(80));

    let _ = response_tx.send(TerminalExecResponse {
        exit_code: 0,
        stdout: stdout_text,
        stderr: String::new(),
        terminal_error: false,
        timed_out: false,
        cwd_after: manager.cwd(),
        interrupted_by_user: false,
        interactive_mode: true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_esc_sequence_for_vi_save_quit() {
        // vi 保存退出：ESC + :wq + CR
        let decoded = decode_terminal_escapes("\\x1b:wq\\r");
        assert_eq!(decoded.as_bytes(), &[0x1b, b':', b'w', b'q', 0x0d]);
    }

    #[test]
    fn decode_ctrl_c_as_etx() {
        // Ctrl+C = ETX (0x03)
        let decoded = decode_terminal_escapes("\\x03");
        assert_eq!(decoded.as_bytes(), &[0x03]);
    }

    #[test]
    fn decode_carriage_return_and_newline() {
        let decoded = decode_terminal_escapes("abc\\r\\n");
        assert_eq!(decoded.as_bytes(), &[b'a', b'b', b'c', 0x0d, 0x0a]);
    }

    #[test]
    fn decode_escape_letter_e() {
        // \e 也表示 ESC
        let decoded = decode_terminal_escapes("\\e");
        assert_eq!(decoded.as_bytes(), &[0x1b]);
    }

    #[test]
    fn decode_preserves_plain_text() {
        // 普通文本无转义，原样返回
        let decoded = decode_terminal_escapes("hello world");
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn decode_unknown_escape_preserved() {
        // 不识别的转义（如 \d）回退原文（保留反斜杠+字符）
        let decoded = decode_terminal_escapes("a\\db");
        assert_eq!(decoded, "a\\db");
    }

    #[test]
    fn decode_literal_backslash() {
        // \\ → 字面反斜杠
        let decoded = decode_terminal_escapes("a\\\\b");
        assert_eq!(decoded, "a\\b");
    }

    #[test]
    fn decode_arrow_key_sequence() {
        // 上方向键 = ESC[A
        let decoded = decode_terminal_escapes("\\x1b[A");
        assert_eq!(decoded.as_bytes(), &[0x1b, b'[', b'A']);
    }

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
    fn collect_matches_marker_with_ansi_residual() {
        // TTY-aware 命令（如 gh）可能在 marker 行前后残留 ANSI 转义片断。
        // strip_ansi + 精确匹配应能命中，而非漏判。
        let manager = make_manager_with_lines(&[
            "\x1b[0m__TIANGONG_START_abc__\x1b[0m",
            "hello",
            "__TIANGONG_CWD_abc__/tmp",
            "__TIANGONG_RC_abc__0",
            "\x1b[0m__TIANGONG_END_abc__",
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
        assert_eq!(out, "hello");
        assert!(!interrupted);
    }

    #[test]
    fn collect_does_not_match_shell_echo_of_wrapper() {
        // shell 回显 wrapper 命令文本时，该行 contains 所有 marker 字符串但
        // strip_ansi + trim 后不等于任何 marker。精确匹配不应误命中回显行，
        // 否则命令尚未执行就判定完成（#237 回归 bug 的根因）。
        let echo_line = "__TIANGONG_= __rc=$?; printf '\\n__TIANGONG_CWD_abc__'; pwd; echo '__TIANGONG_RC_abc__'$__rc; echo '__TIANGONG_END_abc__'";
        let manager = make_manager_with_lines(&[
            "__TIANGONG_START_abc__",
            echo_line, // shell 回显的 wrapper 第二行（contains 所有 marker）
            "actual output",
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
        // 回显行不应被当作 end_marker 提前截断，actual output 必须出现
        assert!(
            out.contains("actual output"),
            "回显行误匹配导致输出截断: {out}"
        );
        assert!(!interrupted);
    }

    #[test]
    fn line_match_ignores_shell_echo() {
        // 直接验证 line_matches_marker 对回显行返回 false
        let echo_line = "__TIANGONG_= echo '__TIANGONG_START_abc__'; export PAGER=cat; cmd";
        assert!(!line_matches_marker(echo_line, "__TIANGONG_START_abc__"));
        // 而真正的 marker 行返回 true
        assert!(line_matches_marker(
            "__TIANGONG_START_abc__",
            "__TIANGONG_START_abc__"
        ));
        // 带 ANSI 残片的 marker 行也返回 true
        assert!(line_matches_marker(
            "\x1b[0m__TIANGONG_START_abc__\x1b[0m",
            "__TIANGONG_START_abc__"
        ));
    }

    #[test]
    fn command_boundary_matches_with_ansi_residual() {
        // command_boundary_seen 用 strip_ansi + 精确匹配，验证带 ANSI 残片的 marker 仍能识别边界。
        let manager = make_manager_with_lines(&[
            "\x1b[0m__TIANGONG_START_abc__",
            "running",
            "__TIANGONG_END_abc__\x1b[0m",
        ]);
        assert!(command_boundary_seen(
            &manager,
            "__TIANGONG_START_abc__",
            "__TIANGONG_END_abc__",
        ));
    }

    #[test]
    fn collect_rc_marker_with_ansi_residual_excluded() {
        // rc_marker 行带 ANSI 残片时，应被正确过滤（不混入命令输出），
        // 且轮询循环能从中解析出退出码（完成判定安全网）。
        let manager = make_manager_with_lines(&[
            "__TIANGONG_START_abc__",
            "command output line",
            "\x1b[0m__TIANGONG_CWD_abc__/tmp\x1b[0m",
            "\x1b[0m__TIANGONG_RC_abc__0",
            // end_marker 缺失（模拟被 ANSI 污染漏判的场景）
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
        // 命令输出应只包含实际内容，cwd/rc marker 行被过滤
        assert_eq!(out, "command output line");
        assert!(!interrupted);
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

    #[cfg(unix)]
    #[test]
    fn non_interactive_command_wrapper_disables_pagers() {
        let (script, invocation) = prepare_non_interactive_command(
            "__TIANGONG_START_x__",
            "git diff HEAD",
            "__TIANGONG_CWD_x__",
            "__TIANGONG_RC_x__",
            "__TIANGONG_END_x__",
        )
        .unwrap();
        let script = script.unwrap();
        let combined = std::fs::read_to_string(script.path()).unwrap();
        assert!(combined.contains("PAGER="));
        assert!(combined.contains("GIT_PAGER="));
        assert!(combined.contains("GH_PAGER="));
        assert!(!combined.contains("GIT_CONFIG_PARAMETERS"));
        assert!(combined.contains("export PAGER=cat GIT_PAGER=cat GH_PAGER=cat LESS=FRX"));
        assert!(combined.contains("unset PAGER"));
        assert!(combined.contains("unset GH_PAGER"));
        assert!(combined.contains("LESS=FRX"));
        assert!(!combined.contains("TERM=dumb"));
        assert!(combined.contains("git diff HEAD"));
        assert!(combined.contains("\ngit diff HEAD\n__TIANGONG_= __rc=$?"));
        assert!(invocation.starts_with(". "));
        assert!(invocation.ends_with('\r'));
        assert!(script
            .path()
            .to_string_lossy()
            .contains("__TIANGONG_START_x___SCRIPT_"));
    }

    #[cfg(unix)]
    #[test]
    fn foreground_stdin_reader_cannot_consume_completion_markers() {
        use std::io::Read as _;
        use std::sync::mpsc;

        use portable_pty::{CommandBuilder, PtySize};

        let start_marker = "__TIANGONG_START_stdin__";
        let end_marker = "__TIANGONG_END_stdin__";
        let (script, combined) = prepare_non_interactive_command(
            start_marker,
            "if IFS= read -r -t 1 stolen; then printf 'stdin-consumed:%s\\n' \"$stolen\"; else echo stdin-empty; fi",
            "__TIANGONG_CWD_stdin__",
            "__TIANGONG_RC_stdin__",
            end_marker,
        )
        .unwrap();
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        #[cfg(target_os = "macos")]
        let mut shell = {
            let mut shell = CommandBuilder::new("/bin/zsh");
            shell.args(["-f", "-i"]);
            shell
        };
        #[cfg(not(target_os = "macos"))]
        let mut shell = {
            let mut shell = CommandBuilder::new("/bin/bash");
            shell.args(["--noprofile", "--norc", "-i"]);
            shell
        };
        shell.env("PS1", "__TIANGONG_TEST_READY__ ");
        shell.env("PS2", "__TIANGONG_TEST_CONTINUE__ ");
        let mut child = pair.slave.spawn_command(shell).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>();
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if output_tx.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut output = String::new();
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !output.contains("__TIANGONG_TEST_READY__") {
            let remaining = ready_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("测试 shell 未就绪");
            output.push_str(&String::from_utf8_lossy(&chunk));
        }

        // 先让 Shell 进入多行续写状态。Ctrl+U 只能清掉当前行，无法取消已经提交的
        // `if`；Ctrl+C 必须让整个多行输入作废，然后就绪探针才能独立执行。
        writer.write_all(b"if true; then\r").unwrap();
        writer.flush().unwrap();
        let continuation_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !output.contains("__TIANGONG_TEST_CONTINUE__") {
            let remaining =
                continuation_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("测试 shell 未进入多行续写状态");
            output.push_str(&String::from_utf8_lossy(&chunk));
        }

        let command_started = std::time::Instant::now();
        let ready_marker = "__TIANGONG_READY_multiline__";
        writer.write_all(b"\x03").unwrap();
        writer.flush().unwrap();
        let mut interrupt_output = String::new();
        let interrupt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !interrupt_output.contains('\n') {
            let remaining = interrupt_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("Shell 未确认 Ctrl+C 已处理");
            let text = String::from_utf8_lossy(&chunk);
            interrupt_output.push_str(&text);
            output.push_str(&text);
        }
        writer
            .write_all(shell_ready_probe(ready_marker).as_bytes())
            .unwrap();
        writer.flush().unwrap();
        let probe_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !output
            .lines()
            .any(|line| line_matches_marker(line, ready_marker))
        {
            let remaining = probe_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("Ctrl+C 后 Shell 未恢复就绪");
            output.push_str(&String::from_utf8_lossy(&chunk));
        }

        writer.write_all(combined.as_bytes()).unwrap();
        writer.flush().unwrap();
        let completed_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let completed = loop {
            if output
                .lines()
                .any(|line| line_matches_marker(line, end_marker))
            {
                break true;
            }
            let remaining = completed_deadline.saturating_duration_since(std::time::Instant::now());
            match output_rx.recv_timeout(remaining) {
                Ok(chunk) => output.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => break false,
            }
        };

        assert!(
            completed,
            "前台程序结束后应立即收到完成标记，PTY 输出: {output}"
        );
        assert!(
            command_started.elapsed() < std::time::Duration::from_secs(4),
            "命令已结束却未及时返回完成标记，PTY 输出: {output}"
        );
        assert!(output.contains("stdin-empty"), "PTY 输出异常: {output}");
        assert!(
            !output.contains("command not found"),
            "残留输入未清理: {output}"
        );
        assert!(!output.contains("stdin-consumed:__TIANGONG_"));
        assert!(!output.contains("parse error"), "PTY 输出异常: {output}");

        drop(script);
        let second_start = "__TIANGONG_START_reuse__";
        let second_end = "__TIANGONG_END_reuse__";
        let (_second_script, second_command) = prepare_non_interactive_command(
            second_start,
            "echo same-shell-reused",
            "__TIANGONG_CWD_reuse__",
            "__TIANGONG_RC_reuse__",
            second_end,
        )
        .unwrap();
        let second_ready = "__TIANGONG_READY_reuse__";
        writer.write_all(b"\x03").unwrap();
        writer.flush().unwrap();
        let mut interrupt_output = String::new();
        let interrupt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !interrupt_output.contains('\n') {
            let remaining = interrupt_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("复用前 Shell 未确认 Ctrl+C 已处理");
            let text = String::from_utf8_lossy(&chunk);
            interrupt_output.push_str(&text);
            output.push_str(&text);
        }
        writer
            .write_all(shell_ready_probe(second_ready).as_bytes())
            .unwrap();
        writer.flush().unwrap();
        let second_ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !output
            .lines()
            .any(|line| line_matches_marker(line, second_ready))
        {
            let remaining =
                second_ready_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("复用前 Shell 未恢复就绪");
            output.push_str(&String::from_utf8_lossy(&chunk));
        }
        writer.write_all(second_command.as_bytes()).unwrap();
        writer.flush().unwrap();
        let reuse_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !output
            .lines()
            .any(|line| line_matches_marker(line, second_end))
        {
            let remaining = reuse_deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = output_rx
                .recv_timeout(remaining)
                .expect("完成后未能复用同一个终端");
            output.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(
            output.contains("same-shell-reused"),
            "PTY 输出异常: {output}"
        );

        let mut filter = crate::output_processor::RawOutputFilter::new();
        let visible = filter.filter(&output);
        assert!(!visible.contains(start_marker), "内部调用泄漏: {visible}");
        assert!(!visible.contains(end_marker), "内部调用泄漏: {visible}");
        assert!(!visible.contains(second_start), "内部调用泄漏: {visible}");
        assert!(!visible.contains(second_end), "内部调用泄漏: {visible}");
        assert!(!visible.contains(ready_marker), "就绪探针泄漏: {visible}");
        assert!(!visible.contains(second_ready), "就绪探针泄漏: {visible}");

        let _ = child.kill();
        let _ = child.wait();
        drop(writer);
        drop(pair.master);
        reader_thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_interactive_wrapper_isolates_errexit_from_long_lived_shell() {
        let (_script, combined) = prepare_non_interactive_command(
            "__TIANGONG_START_x__",
            "set -e\nfalse\necho should-not-run",
            "__TIANGONG_CWD_x__",
            "__TIANGONG_RC_x__",
            "__TIANGONG_END_x__",
        )
        .unwrap();
        let script_path = combined.trim_end_matches('\r').strip_prefix(". ").unwrap();

        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(". {script_path}"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "父 shell 被 errexit 带走: {stdout}"
        );
        assert!(stdout.contains("__TIANGONG_RC_x__1"));
        assert!(stdout.contains("__TIANGONG_END_x__"));
        assert!(!stdout.contains("should-not-run\n"));
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_commands_keep_current_shell_semantics() {
        assert!(!command_requires_isolation("cd /tmp\nexport MODE=test"));
        assert!(command_requires_isolation("set -euo pipefail\nfalse"));
        assert!(command_requires_isolation("set -o errexit; false"));
        assert!(command_requires_isolation("setopt ERR_EXIT\nfalse"));
        assert!(command_requires_isolation("trap 'echo done' EXIT"));
        assert!(command_requires_isolation("return 1"));
        assert!(command_requires_isolation("exec cargo check"));
    }

    #[cfg(unix)]
    #[test]
    fn force_stop_prevents_sigint_ignoring_command_from_writing_after_release() {
        use std::os::unix::process::CommandExt;

        let temp = tempfile::tempdir().unwrap();
        let ready_path = temp.path().join("ready");
        let late_write_path = temp.path().join("late-write");
        let script = format!(
            "trap '' INT HUP TERM; printf ready > {}; sleep 1; printf late > {}",
            crate::util::shell_quote(&ready_path.to_string_lossy()),
            crate::util::shell_quote(&late_write_path.to_string_lossy()),
        );
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .process_group(0)
            .spawn()
            .unwrap();
        let process_group = child.id() as libc::pid_t;

        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !ready_path.exists() && std::time::Instant::now() < ready_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ready_path.exists(), "测试命令未进入忽略 SIGINT 的执行阶段");

        let sent = unsafe { libc::kill(-process_group, libc::SIGINT) };
        assert_eq!(sent, 0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            child.try_wait().unwrap().is_none(),
            "测试命令必须证明普通 Ctrl+C 不足以停止它"
        );

        crate::manager::force_stop_process_group(process_group).unwrap();
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(
            !late_write_path.exists(),
            "取消栅栏放行后，忽略 SIGINT 的命令不得继续写文件"
        );
    }

    /// #237 完整复现链路验证（不依赖网络/GitHub 登录）。
    ///
    /// 模拟一个 TTY-aware 命令（类似 gh）在 PTY 下的完整输出流：
    /// 1. spinner 动画（隐藏/显示光标 + CR/清行）
    /// 2. 彩色输出（SGR 序列 + 真彩色冒号参数）
    /// 3. shell 回显 wrapper 命令文本（contains 所有 marker）
    /// 4. 实际的 marker 输出（start/cwd/rc/end）
    ///
    /// 验证：marker 检测只命中实际 marker 行（不命中回显），
    /// 命令输出完整收集，退出码正确解析。
    #[test]
    fn end_to_end_tty_aware_command_marker_detection() {
        use crate::output_processor::TerminalLineProcessor;

        let marker_id = "test_e2e_001";
        let start = format!("__TIANGONG_START_{marker_id}__");
        let end = format!("__TIANGONG_END_{marker_id}__");
        let cwd = format!("__TIANGONG_CWD_{marker_id}__");
        let rc = format!("__TIANGONG_RC_{marker_id}__");

        // 构造 wrapper 命令文本（会被 shell 回显，contains 所有 marker）
        let wrapper_echo =
            format!("__TIANGONG_= __rc=$?; printf '\\n{cwd}'; pwd; echo '{rc}'$__rc; echo '{end}'");

        // 模拟 PTY 完整输出流（shell 回显 + spinner + 彩色命令输出 + marker）
        let start_echo = format!("__TIANGONG_= echo '{start}'; export PAGER=cat; fake_gh_cmd\r\n");
        let wrapper_echo_line = format!("{wrapper_echo}\r\n");
        let spinner = "\x1b[?25l\r\x1b[K\r⣾\r\x1b[K\r⣽\r\x1b[K\x1b[?25h\r\x1b[K";
        let color_error = "\x1b[38:2::255:0:0mError:\x1b[0m something failed\r\n";
        let color_ok = "\x1b[32mOK\x1b[0m done\r\n";
        let cwd_line = format!("{cwd}/tmp\r\n");
        let rc_line = format!("{rc}1\r\n");
        let end_line = format!("{end}\r\n");
        let pty_output = format!(
            "{start_echo}{wrapper_echo_line}{spinner}{color_error}{color_ok}{cwd_line}{rc_line}{end_line}"
        );

        // 用 TerminalLineProcessor 处理（模拟 output_reader 线程）
        let mut processor = TerminalLineProcessor::new();
        let lines = processor.process(&pty_output);

        // 收集到的行里必须包含命令输出
        let has_error = lines.iter().any(|l| l.contains("Error:"));
        let has_ok = lines.iter().any(|l| l == "OK done");
        assert!(has_error, "命令彩色输出丢失！lines: {:?}", lines);
        assert!(has_ok, "命令输出丢失！lines: {:?}", lines);

        // 回显的 wrapper 文本不应等于任何 marker（精确匹配不会误判）
        let echo_line = lines.iter().find(|l| l.contains("__rc=$?"));
        if let Some(echo) = echo_line {
            assert!(
                !line_matches_marker(echo, &end),
                "回显行被误判为 end marker！echo: {echo}"
            );
            assert!(
                !line_matches_marker(echo, &rc),
                "回显行被误判为 rc marker！echo: {echo}"
            );
        }

        // 实际 marker 行必须能被精确匹配
        let rc_line = lines
            .iter()
            .find(|l| extract_marker_value(l, rc.as_str()).is_some())
            .expect("rc marker 行未找到");
        let rc_value = extract_marker_value(rc_line, rc.as_str()).unwrap();
        assert_eq!(rc_value, "1", "退出码解析错误：{rc_value}");

        let cwd_line = lines
            .iter()
            .find(|l| extract_marker_value(l, cwd.as_str()).is_some())
            .expect("cwd marker 行未找到");
        let cwd_value = extract_marker_value(cwd_line, cwd.as_str()).unwrap();
        assert_eq!(cwd_value, "/tmp", "cwd 解析错误：{cwd_value}");

        // end marker 必须能精确匹配
        assert!(
            lines.iter().any(|l| line_matches_marker(l, &end)),
            "end marker 行未找到！lines: {:?}",
            lines
        );
    }
}
