//! Windows AppContainer 沙箱。
//!
//! Launcher 为每次调用创建独立 AppContainer 身份，只向目标程序、工作区与
//! 专用临时目录授予该身份所需的最小 ACL。目标以挂起状态创建，加入带资源
//! 配额的 Job Object 后才恢复运行；网络能力只在策略明确允许时加入。

#![cfg(windows)]

use std::collections::HashSet;
use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_SHARING_VIOLATION, ERROR_SUCCESS,
    GetHandleInformation, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    LocalFree, SetHandleInformation, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    BuildTrusteeWithSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
    REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION, FreeSid, NO_INHERITANCE,
    OBJECT_INHERIT_ACE, PSID, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
    WinCapabilityInternetClientSid, WinCapabilityPrivateNetworkClientServerSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, GetFileInformationByHandle,
    OPEN_EXISTING, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, OpenEventW, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    ReleaseMutex, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, SYNCHRONIZATION_SYNCHRONIZE,
    TerminateProcess, UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
};

use super::{SandboxAvailability, SandboxMode, SandboxPolicy, SandboxResourceLimits};

const SE_GROUP_ENABLED: u32 = 4;
const ACL_MUTATION_MUTEX: &str = "Local\\TiangongSandboxAclMutation-v1";
const FILE_WRITE_ACCESS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER;
const FILE_WORKSPACE_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
const FILE_PROGRAM_ACCESS: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

/// Windows Launcher 的原生启动参数。
pub struct WindowsLaunchRequest<'a> {
    pub program: &'a Path,
    pub program_root: &'a Path,
    pub args: &'a [String],
    pub policy: &'a SandboxPolicy,
    pub host_pid: Option<u32>,
    pub stop_event_name: Option<&'a str>,
    pub timeout: Option<Duration>,
}

pub fn availability() -> SandboxAvailability {
    let name = wide("TiangongSandbox.AvailabilityProbe");
    let mut sid = std::ptr::null_mut();
    let result = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    if result < 0 {
        return SandboxAvailability::Unsupported(format!(
            "系统无法创建 AppContainer 身份（HRESULT=0x{:08x}）",
            result as u32
        ));
    }
    unsafe {
        FreeSid(sid);
    }
    SandboxAvailability::Available
}

/// 创建、运行并清理一次 Windows AppContainer 调用。
pub fn launch(request: WindowsLaunchRequest<'_>) -> Result<i32> {
    validate_launch_request(&request)?;
    let capabilities = CapabilitySet::new(request.policy.allow_network)?;
    let mut profile = AppContainerProfile::create(&capabilities)?;
    let mut grants = AclGrants::apply(
        profile.sid,
        request.program,
        request.program_root,
        request.policy,
    )?;

    let execution = run_restricted_process(&request, &profile, &capabilities);
    let acl_cleanup = grants.revoke();
    let profile_cleanup = profile.delete();

    match (execution, acl_cleanup, profile_cleanup) {
        (Ok(code), Ok(()), Ok(())) => Ok(code),
        (Err(error), Ok(()), Ok(())) => Err(error),
        (execution, acl_cleanup, profile_cleanup) => {
            let mut messages = Vec::new();
            if let Err(error) = execution {
                messages.push(format!("目标执行失败: {error:#}"));
            }
            if let Err(error) = acl_cleanup {
                messages.push(format!("撤销临时目录授权失败: {error:#}"));
            }
            if let Err(error) = profile_cleanup {
                messages.push(format!("删除临时 AppContainer 身份失败: {error:#}"));
            }
            bail!(messages.join("; "))
        }
    }
}

/// 由受限目标自检自身所在 Job 的资源上限。
pub fn current_process_limits_match(expected: SandboxResourceLimits) -> bool {
    let mut actual = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let queried = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut actual).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    queried != 0
        && actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0
        && actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0
        && actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_TIME != 0
        && actual.BasicLimitInformation.PerJobUserTimeLimit
            == expected.max_cpu_time_seconds as i64 * 10_000_000
        && actual.BasicLimitInformation.ActiveProcessLimit == expected.max_processes
        && actual.JobMemoryLimit == expected.max_memory_bytes as usize
}

fn validate_launch_request(request: &WindowsLaunchRequest<'_>) -> Result<()> {
    if request.policy.mode == SandboxMode::FullAccess {
        bail!("Windows Launcher 不接受 full_access 策略");
    }
    if !request.program.is_absolute() || !request.program_root.is_absolute() {
        bail!("Windows 目标程序及根目录必须是绝对路径");
    }
    if !request.program.is_file() || !request.program_root.is_dir() {
        bail!("Windows 目标程序或根目录不存在");
    }
    for root in request.policy.writable_roots() {
        validate_writable_tree(&root)
            .with_context(|| format!("Windows 可写目录不安全: {}", root.display()))?;
    }
    Ok(())
}

fn validate_writable_tree(root: &Path) -> Result<()> {
    if !root.is_absolute() || !root.is_dir() {
        bail!("可写根必须是已存在的绝对目录");
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("读取路径元数据失败: {}", path.display()))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("可写目录包含重解析点，拒绝授予权限: {}", path.display());
        }
        if metadata.is_file() && file_link_count(&path)? > 1 {
            bail!("可写目录包含硬链接，拒绝授予权限: {}", path.display());
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)
                .with_context(|| format!("扫描可写目录失败: {}", path.display()))?
            {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}

fn file_link_count(path: &Path) -> Result<u32> {
    let path = wide_os(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("打开文件以检查硬链接失败");
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(raw_handle(&handle), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("读取文件硬链接数量失败");
    }
    Ok(info.nNumberOfLinks)
}

struct CapabilitySet {
    _sid_storage: Vec<Box<[u8]>>,
    entries: Vec<SID_AND_ATTRIBUTES>,
}

impl CapabilitySet {
    fn new(allow_network: bool) -> Result<Self> {
        let mut sid_storage = Vec::new();
        if allow_network {
            for kind in [
                WinCapabilityInternetClientSid,
                WinCapabilityPrivateNetworkClientServerSid,
            ] {
                let mut storage = vec![0u8; SECURITY_MAX_SID_SIZE as usize].into_boxed_slice();
                let mut size = storage.len() as u32;
                if unsafe {
                    CreateWellKnownSid(
                        kind,
                        std::ptr::null_mut(),
                        storage.as_mut_ptr().cast(),
                        &mut size,
                    )
                } == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("创建 AppContainer 网络能力 SID 失败");
                }
                sid_storage.push(storage);
            }
        }
        let entries = sid_storage
            .iter_mut()
            .map(|storage| SID_AND_ATTRIBUTES {
                Sid: storage.as_mut_ptr().cast(),
                Attributes: SE_GROUP_ENABLED,
            })
            .collect();
        Ok(Self {
            _sid_storage: sid_storage,
            entries,
        })
    }

    fn as_ptr(&self) -> *const SID_AND_ATTRIBUTES {
        if self.entries.is_empty() {
            std::ptr::null()
        } else {
            self.entries.as_ptr()
        }
    }
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: PSID,
    active: bool,
}

impl AppContainerProfile {
    fn create(capabilities: &CapabilitySet) -> Result<Self> {
        let name = wide(&format!("TiangongSandbox.{}", scru128::new()));
        let display = wide("Tiangong Sandbox");
        let description = wide("Temporary Tiangong sandbox identity");
        let mut sid = std::ptr::null_mut();
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                capabilities.as_ptr(),
                capabilities.entries.len() as u32,
                &mut sid,
            )
        };
        if result < 0 {
            bail!(
                "创建临时 AppContainer 身份失败（HRESULT=0x{:08x}）",
                result as u32
            );
        }
        Ok(Self {
            name,
            sid,
            active: true,
        })
    }

    fn delete(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        for attempt in 0..10 {
            let result = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
            if result >= 0 {
                self.active = false;
                return Ok(());
            }
            if attempt == 9 {
                bail!(
                    "删除 AppContainer Profile 失败（HRESULT=0x{:08x}）",
                    result as u32
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unreachable!()
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                DeleteAppContainerProfile(self.name.as_ptr());
            }
        }
        if !self.sid.is_null() {
            unsafe {
                FreeSid(self.sid);
            }
        }
    }
}

struct AclRoot {
    path: PathBuf,
    recursive: bool,
}

struct AclGrants {
    sid: PSID,
    roots: Vec<AclRoot>,
    active: bool,
}

impl AclGrants {
    fn apply(
        sid: PSID,
        program: &Path,
        program_root: &Path,
        policy: &SandboxPolicy,
    ) -> Result<Self> {
        let mut grants = Self {
            sid,
            roots: Vec::new(),
            active: true,
        };
        let result = (|| {
            grants.add_ancestor_traversal(sid, program_root)?;
            grants.add(
                sid,
                program_root,
                FILE_PROGRAM_ACCESS,
                NO_INHERITANCE,
                false,
            )?;
            grants.add(sid, program, FILE_PROGRAM_ACCESS, NO_INHERITANCE, false)?;

            let writable = policy.writable_roots();
            for root in &writable {
                grants.add_ancestor_traversal(sid, root)?;
                grants.add(
                    sid,
                    root,
                    FILE_WORKSPACE_ACCESS,
                    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    true,
                )?;
            }
            for path in policy.read_only_roots() {
                if path.exists() {
                    grants.deny(
                        sid,
                        &path,
                        FILE_WRITE_ACCESS,
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    )?;
                }
            }
            for path in policy.denied_read_roots() {
                if path.exists() && writable.iter().any(|root| path.starts_with(root)) {
                    grants.deny(
                        sid,
                        &path,
                        FILE_ALL_ACCESS,
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    )?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let cleanup = grants.revoke();
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow!("{error:#}; 回滚临时 ACL 失败: {cleanup:#}")),
            };
        }
        Ok(grants)
    }

    fn add_ancestor_traversal(&mut self, sid: PSID, path: &Path) -> Result<()> {
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.as_os_str().is_empty() {
                match self.add(sid, ancestor, FILE_TRAVERSE, NO_INHERITANCE, false) {
                    Ok(()) => {}
                    Err(error) if is_locked_ancestor(&error) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    fn add(
        &mut self,
        sid: PSID,
        path: &Path,
        permissions: u32,
        inheritance: u32,
        recursive: bool,
    ) -> Result<()> {
        modify_acl(path, sid, permissions, inheritance, GRANT_ACCESS)?;
        self.roots.push(AclRoot {
            path: path.to_path_buf(),
            recursive,
        });
        Ok(())
    }

    fn deny(&mut self, sid: PSID, path: &Path, permissions: u32, inheritance: u32) -> Result<()> {
        modify_acl(path, sid, permissions, inheritance, DENY_ACCESS)?;
        self.roots.push(AclRoot {
            path: path.to_path_buf(),
            recursive: true,
        });
        Ok(())
    }

    fn revoke(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut paths = Vec::new();
        for root in &self.roots {
            if root.recursive {
                collect_tree_paths(&root.path, &mut paths);
            } else {
                paths.push(root.path.clone());
            }
        }
        paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        let mut seen = HashSet::new();
        let mut failures = Vec::new();
        for path in paths {
            if !seen.insert(path.clone()) || !path.exists() {
                continue;
            }
            if let Err(error) = modify_acl(&path, self.sid, 0, NO_INHERITANCE, REVOKE_ACCESS) {
                failures.push(format!("{}: {error:#}", path.display()));
            }
        }
        if failures.is_empty() {
            self.active = false;
            Ok(())
        } else {
            bail!(failures.join("; "))
        }
    }
}

impl Drop for AclGrants {
    fn drop(&mut self) {
        if self.active {
            let _ = self.revoke();
        }
    }
}

fn collect_tree_paths(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(metadata) = std::fs::symlink_metadata(root) else {
        return;
    };
    paths.push(root.to_path_buf());
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            collect_tree_paths(&entry.path(), paths);
        }
    }
}

fn modify_acl(path: &Path, sid: PSID, permissions: u32, inheritance: u32, mode: i32) -> Result<()> {
    let _mutation_lock = AclMutationLock::acquire()?;
    let path_wide = wide_os(path.as_os_str());
    let mut old_acl = std::ptr::null_mut();
    let mut security_descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_acl,
            std::ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error(status)).context("读取 ACL 失败");
    }

    let mut trustee = Default::default();
    unsafe {
        BuildTrusteeWithSidW(&mut trustee, sid);
    }
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: permissions,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: trustee,
    };
    let mut new_acl = std::ptr::null_mut();
    let acl_status = unsafe { SetEntriesInAclW(1, &access, old_acl, &mut new_acl) };
    if acl_status != ERROR_SUCCESS {
        unsafe {
            LocalFree(security_descriptor);
        }
        return Err(win32_error(acl_status)).context("构造 ACL 失败");
    }
    let set_status = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_acl,
            std::ptr::null(),
        )
    };
    unsafe {
        LocalFree(new_acl.cast());
        LocalFree(security_descriptor);
    }
    if set_status != ERROR_SUCCESS {
        return Err(win32_error(set_status)).context("写入 ACL 失败");
    }
    Ok(())
}

fn win32_error(status: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::from_ne_bytes(status.to_ne_bytes()))
}

fn is_locked_ancestor(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
            .is_some_and(|code| {
                code == ERROR_ACCESS_DENIED as i32 || code == ERROR_SHARING_VIOLATION as i32
            })
    })
}

struct AclMutationLock(OwnedHandle);

impl AclMutationLock {
    fn acquire() -> Result<Self> {
        let name = wide(ACL_MUTATION_MUTEX);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("创建 Windows ACL 更新互斥锁失败");
        }
        let lock = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        match unsafe { WaitForSingleObject(raw_handle(&lock.0), 30_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(lock),
            WAIT_TIMEOUT => bail!("等待 Windows ACL 更新互斥锁超时"),
            WAIT_FAILED => {
                Err(std::io::Error::last_os_error()).context("等待 Windows ACL 更新互斥锁失败")
            }
            status => bail!("等待 Windows ACL 更新互斥锁返回异常状态: {status}"),
        }
    }
}

impl Drop for AclMutationLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(raw_handle(&self.0));
        }
    }
}

fn run_restricted_process(
    request: &WindowsLaunchRequest<'_>,
    profile: &AppContainerProfile,
    capabilities: &CapabilitySet,
) -> Result<i32> {
    let job = WindowsJob::new(request.policy.resource_limits)?;
    let host = request.host_pid.map(open_host_process).transpose()?;
    let stop_event = request.stop_event_name.map(open_stop_event).transpose()?;

    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: capabilities.as_ptr().cast_mut(),
        CapabilityCount: capabilities.entries.len() as u32,
        Reserved: 0,
    };
    let stdio = InheritableStdio::prepare()?;
    let mut attributes = ProcessAttributes::new(&security_capabilities, &stdio.handles)?;

    let application = wide_os(request.program.as_os_str());
    let mut command_line = windows_command_line(request.program.as_os_str(), request.args);
    let cwd = if request.policy.mode == SandboxMode::WorkspaceWrite {
        request.policy.workspace.as_path()
    } else {
        request.program_root
    };
    let cwd = wide_os(cwd.as_os_str());
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio.handles[0];
    startup.StartupInfo.hStdOutput = stdio.handles[1];
    startup.StartupInfo.hStdError = stdio.handles[2];
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut process_info = PROCESS_INFORMATION::default();

    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            cwd.as_ptr(),
            (&raw const startup.StartupInfo),
            &mut process_info,
        )
    };
    drop(stdio);
    if created == 0 {
        return Err(std::io::Error::last_os_error()).context("创建 AppContainer 目标进程失败");
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
    if unsafe { AssignProcessToJobObject(job.raw(), raw_handle(&process)) } == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            TerminateProcess(raw_handle(&process), 1);
            WaitForSingleObject(raw_handle(&process), INFINITE);
        }
        return Err(error).context("将 AppContainer 进程加入 Job 失败");
    }
    if unsafe { ResumeThread(raw_handle(&thread)) } == u32::MAX {
        let error = std::io::Error::last_os_error();
        job.terminate_and_wait()
            .context("恢复线程失败后清理 AppContainer Job 失败")?;
        return Err(error).context("恢复 AppContainer 主线程失败");
    }
    drop(thread);

    let mut handles = vec![raw_handle(&process)];
    if let Some(handle) = &host {
        handles.push(raw_handle(handle));
    }
    if let Some(handle) = &stop_event {
        handles.push(raw_handle(handle));
    }
    let wait_timeout = request
        .timeout
        .map(|timeout| timeout.as_millis().min((INFINITE - 1) as u128) as u32)
        .unwrap_or(INFINITE);
    let wait =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, wait_timeout) };
    if wait == WAIT_FAILED {
        let error = std::io::Error::last_os_error();
        job.terminate_and_wait()
            .context("等待失败后清理 AppContainer Job 失败")?;
        return Err(error).context("等待 AppContainer 进程失败");
    }
    if wait == WAIT_TIMEOUT {
        job.terminate_and_wait()
            .context("超时后清理 AppContainer Job 失败")?;
        bail!("等待 AppContainer 进程超过 {:?}", request.timeout.unwrap());
    }
    let target_exited = wait == WAIT_OBJECT_0;
    let mut exit_code = 1;
    if target_exited && unsafe { GetExitCodeProcess(raw_handle(&process), &mut exit_code) } == 0 {
        let error = std::io::Error::last_os_error();
        job.terminate_and_wait()
            .context("读取退出码失败后清理 AppContainer Job 失败")?;
        return Err(error).context("读取 AppContainer 退出码失败");
    }
    job.terminate_and_wait()?;
    if unsafe { WaitForSingleObject(raw_handle(&process), INFINITE) } == WAIT_FAILED {
        return Err(std::io::Error::last_os_error()).context("等待 AppContainer 清理失败");
    }
    Ok(exit_code as i32)
}

struct WindowsJob(OwnedHandle);

impl WindowsJob {
    fn new(limits: SandboxResourceLimits) -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("创建 Windows Job Object 失败");
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_JOB_TIME;
        info.BasicLimitInformation.PerJobUserTimeLimit =
            limits.max_cpu_time_seconds.saturating_mul(10_000_000) as i64;
        info.BasicLimitInformation.ActiveProcessLimit = limits.max_processes;
        info.JobMemoryLimit = limits.max_memory_bytes as usize;
        if unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("配置 Windows Job 配额失败");
        }
        Ok(job)
    }

    fn raw(&self) -> HANDLE {
        raw_handle(&self.0)
    }

    fn terminate_and_wait(&self) -> Result<()> {
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(std::io::Error::last_os_error()).context("终止 Windows Sandbox Job 失败");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.raw(),
                    JobObjectBasicAccountingInformation,
                    (&raw mut accounting).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("读取 Windows Sandbox Job 清理状态失败");
            }
            if accounting.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "Windows Sandbox Job 清理超时，仍有 {} 个进程",
                    accounting.ActiveProcesses
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct ProcessAttributes {
    storage: Vec<usize>,
}

impl ProcessAttributes {
    fn new(capabilities: &SECURITY_CAPABILITIES, inherited_handles: &[HANDLE]) -> Result<Self> {
        let mut bytes = 0usize;
        let initialized =
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut bytes) };
        if initialized != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(std::io::Error::last_os_error())
                .context("计算 Windows 进程属性列表长度失败");
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let pointer = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(pointer, 2, 0, &mut bytes) } == 0 {
            return Err(std::io::Error::last_os_error()).context("初始化 Windows 进程属性失败");
        }
        if unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (capabilities as *const SECURITY_CAPABILITIES).cast(),
                size_of::<SECURITY_CAPABILITIES>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            unsafe {
                DeleteProcThreadAttributeList(pointer);
            }
            return Err(std::io::Error::last_os_error()).context("应用 AppContainer 安全属性失败");
        }
        if unsafe {
            UpdateProcThreadAttribute(
                pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited_handles.as_ptr().cast(),
                size_of_val(inherited_handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            unsafe {
                DeleteProcThreadAttributeList(pointer);
            }
            return Err(std::io::Error::last_os_error()).context("限制 AppContainer 继承句柄失败");
        }
        Ok(Self { storage })
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for ProcessAttributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.as_mut_ptr());
        }
    }
}

struct InheritableStdio {
    handles: [HANDLE; 3],
    previous: [u32; 3],
    changed: usize,
}

impl InheritableStdio {
    fn prepare() -> Result<Self> {
        let handles = unsafe {
            [
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
                GetStdHandle(STD_ERROR_HANDLE),
            ]
        };
        let mut previous = [0u32; 3];
        for (index, handle) in handles.iter().copied().enumerate() {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                bail!("Launcher 缺少可继承的标准输入输出句柄");
            }
            if unsafe { GetHandleInformation(handle, &mut previous[index]) } == 0 {
                return Err(std::io::Error::last_os_error()).context("读取 stdio 句柄标志失败");
            }
        }
        let mut prepared = Self {
            handles,
            previous,
            changed: 0,
        };
        for handle in prepared.handles.iter().copied() {
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
                == 0
            {
                return Err(std::io::Error::last_os_error()).context("设置 stdio 句柄继承失败");
            }
            prepared.changed += 1;
        }
        Ok(prepared)
    }
}

impl Drop for InheritableStdio {
    fn drop(&mut self) {
        for (handle, previous) in self
            .handles
            .iter()
            .copied()
            .zip(self.previous)
            .take(self.changed)
        {
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, previous & HANDLE_FLAG_INHERIT);
            }
        }
    }
}

fn open_host_process(pid: u32) -> Result<OwnedHandle> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("打开宿主进程监视句柄失败");
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn open_stop_event(name: &str) -> Result<OwnedHandle> {
    let name = wide(name);
    let handle = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("打开 Sandbox 停止事件失败");
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn windows_command_line(program: &OsStr, args: &[String]) -> Vec<u16> {
    let mut result = Vec::new();
    push_windows_arg(&mut result, program.encode_wide());
    for arg in args {
        result.push(b' ' as u16);
        push_windows_arg(&mut result, arg.encode_utf16());
    }
    result.push(0);
    result
}

fn push_windows_arg<I>(output: &mut Vec<u16>, value: I)
where
    I: IntoIterator<Item = u16>,
{
    let value = value.into_iter().collect::<Vec<_>>();
    let needs_quotes = value.is_empty()
        || value
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16 || *unit == b'"' as u16);
    if !needs_quotes {
        output.extend(value);
        return;
    }
    output.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in value {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            output.push(unit);
            backslashes = 0;
        } else {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            output.push(unit);
            backslashes = 0;
        }
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    output.push(b'"' as u16);
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn wide_os(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[test]
    fn windows_argument_quoting_handles_spaces_quotes_and_backslashes() {
        let line = windows_command_line(
            OsStr::new(r"C:\Program Files\tool.exe"),
            &[r#"a\"b"#.to_string(), r"tail\".to_string()],
        );
        let text = OsString::from_wide(&line[..line.len() - 1])
            .to_string_lossy()
            .into_owned();
        assert_eq!(text, r#""C:\Program Files\tool.exe" "a\"b" tail\"#);
    }
}
