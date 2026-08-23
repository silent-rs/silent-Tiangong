//! Windows 受限令牌沙箱（RFC 0017 S6，v1：核心原语就绪，待真实 Windows 环境验证）。
//!
//! 方案：`CreateRestrictedToken(LUA_TOKEN)` 把子进程降到 Low 完整性级别——
//! Low IL 进程**可以读取**绝大多数用户文件，但**写入中完整性对象（用户文件、
//! 注册表、程序目录）被 ACL 拒绝**，与"读全盘放行、写受限"的目标语义对齐
//! （Codex 同族思路的轻量实现；完整 deny-SID 写白名单见 RFC 开放问题）。
//!
//! S6 v1 边界（诚实标注）：
//! - 提供 [`spawn_low_integrity`] 原语与 [`LowIntegrityChild`] 的 wait/terminate；
//! - **stdout/stderr 管道捕获未实现**（需 PROC_THREAD_ATTRIBUTE_HANDLE_LIST
//!   句柄继承），command sidecar 依赖输出捕获，故 [`crate::sandbox::wrap`]
//!   在 Windows 仍返回降级直跑（快照层兜底）；
//! - 网络不在限制范围（RFC §13 已知缺口）；
//! - 全部代码经 `cargo check --target x86_64-pc-windows-msvc` 验证类型，
//!   运行时行为待真实 Windows 环境验证。

#![cfg(target_os = "windows")]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows_sys::Win32::Security::{
    DISABLE_MAX_PRIVILEGE, LUA_TOKEN, SECURITY_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY,
    TokenPrimary,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, CreateRestrictedToken, GetExitCodeProcess, INFINITE, OpenProcessToken,
    PROCESS_INFORMATION, STARTUPINFOW, TERMINATE_PROCESS, WaitForSingleObject,
};

/// 受限（Low 完整性）子进程句柄：等待、退出码与终止。
pub struct LowIntegrityChild {
    process: HANDLE,
    pub pid: u32,
}

impl LowIntegrityChild {
    /// 阻塞等待退出，返回退出码。
    pub fn wait(&self) -> io::Result<i32> {
        // SAFETY: 句柄来自 CreateProcessAsUserW 且未被关闭。
        let wait = unsafe { WaitForSingleObject(self.process, INFINITE) };
        if wait == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut exit_code: u32 = 0;
        // SAFETY: 出参为本地变量。
        if unsafe { GetExitCodeProcess(self.process, &mut exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(exit_code as i32)
    }

    /// 终止进程。
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: 句柄有效；退出码任意。
        if unsafe { windows_sys::Win32::System::Threading::TerminateProcess(self.process, 1) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for LowIntegrityChild {
    fn drop(&mut self) {
        // SAFETY: 句柄生命周期归本结构。
        unsafe { CloseHandle(self.process) };
    }
}

/// 以 Low 完整性级别 spawn 子进程（无管道捕获，输出为终端继承或丢弃）。
pub fn spawn_low_integrity(
    program: &OsStr,
    args: &[String],
    cwd: Option<&OsStr>,
) -> io::Result<LowIntegrityChild> {
    // 1) 由当前进程令牌派生 Low IL 受限令牌。
    // SAFETY: GetCurrentProcess 返回伪句柄。
    let current_process: HANDLE =
        unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() };
    let mut current_token: HANDLE = std::ptr::null_mut();
    // SAFETY: 句柄来自 GetCurrentProcess；出参为本地变量。
    if unsafe {
        OpenProcessToken(
            current_process,
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut current_token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut restricted_token: HANDLE = std::ptr::null_mut();
    // SAFETY: 输入句柄有效；LUA_TOKEN 生成降完整性令牌，无 SID 增删。
    let restrict_result = unsafe {
        CreateRestrictedToken(
            current_token,
            LUA_TOKEN | DISABLE_MAX_PRIVILEGE,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            TokenPrimary,
            &mut restricted_token,
        )
    };
    // SAFETY: 打开的令牌句柄按需关闭。
    unsafe { CloseHandle(current_token) };
    if restrict_result == 0 {
        return Err(io::Error::last_os_error());
    }

    // 2) 组装命令行（Windows 引号拼接）。
    let mut command_line = String::new();
    push_quoted(&mut command_line, &program.to_string_lossy());
    for arg in args {
        command_line.push(' ');
        push_quoted(&mut command_line, arg);
    }
    let mut command_line_utf16: Vec<u16> = command_line.encode_utf16().chain([0]).collect();

    // 3) spawn（句柄不继承——管道捕获见模块文档的边界说明）。
    let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup_info.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let cwd_utf16: Option<Vec<u16>> = cwd.map(|value| value.encode_utf16().chain([0]).collect());
    let mut security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 0,
    };

    // SAFETY: 指针均指向已初始化的本地缓冲；环境块为空（继承默认）。
    let created = unsafe {
        CreateProcessAsUserW(
            restricted_token,
            std::ptr::null(),
            command_line_utf16.as_mut_ptr(),
            &mut security_attributes,
            &mut security_attributes,
            0,
            0,
            std::ptr::null(),
            cwd_utf16
                .as_ref()
                .map_or(std::ptr::null(), |dir| dir.as_ptr()),
            &mut startup_info as *mut STARTUPINFOW,
            &mut process_info as *mut PROCESS_INFORMATION,
        )
    };
    // SAFETY: 派生令牌用毕即关。
    unsafe { CloseHandle(restricted_token) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }

    let pid = process_info.dwProcessId;
    // SAFETY: 线程句柄不再需要。
    unsafe { CloseHandle(process_info.hThread) };
    Ok(LowIntegrityChild {
        process: process_info.hProcess,
        pid,
    })
}

fn push_quoted(out: &mut String, value: &str) {
    if value.is_empty() {
        out.push_str("\"\"");
        return;
    }
    let needs_quote = value.contains(' ') || value.contains('\t');
    if needs_quote {
        out.push('"');
    }
    out.push_str(value);
    if needs_quote {
        out.push('"');
    }
}
