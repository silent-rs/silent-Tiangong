//! Leader 选举与进程单例。
//!
//! 通用 sidecar 单例机制（对齐 Memory / MCP / Index sidecar 的选举）：
//! - `leader.lock` 原子创建实现单 Leader 互斥
//! - `leader.json` 记录 leader 服务信息与心跳
//! - 已有健康 leader 时本 sidecar 优雅退出（不重复运行）
//!
//! sidecar 由运行时按 `plugin.json` 启动，运行时通过 endpoint 文件连接 winner。

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tiangong_plugin_runtime::protocol::{Request, Response};

use crate::endpoint;
use crate::identity::SidecarConfig;
use crate::server::{self, IpcBridge};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT_SECS: i64 = 10;
static LEADER_INFO_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 业务 sidecar 实现此 trait，提供请求分发。
///
/// `dispatch` 在 IPC server 的 tokio task 内 `await` 调用，慢操作应在内部用
/// `spawn_blocking` 等让出调度（参见 index sidecar）。
#[async_trait::async_trait]
pub trait SidecarService: Send + Sync {
    async fn dispatch(&self, request: Request) -> Response;
}

/// Leader 状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderState {
    /// 本进程是 Leader（持有 lock 文件并维护心跳）。
    Leader,
    /// 已有 Leader 运行中，本进程无需重复启动。
    Follower { pid: u32 },
}

/// leader.json 中的注册信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderInfo {
    pub pid: u32,
    pub service: String,
    pub started_at: String,
    pub heartbeat_at: String,
    /// 传输协议版本（版本不匹配的旧 leader 视为需替换）。
    #[serde(default)]
    pub protocol_version: String,
}

impl LeaderInfo {
    fn new(service: String, protocol_version: String) -> Self {
        let now = chrono::Local::now().naive_local().to_string();
        Self {
            pid: std::process::id(),
            service,
            started_at: now.clone(),
            heartbeat_at: now,
            protocol_version,
        }
    }
}

/// 带运行时守卫的 sidecar（持有选举结果 + IPC bridge + 心跳线程）。
pub struct ManagedSidecar {
    inner: Arc<Mutex<ManagedSidecarInner>>,
}

struct ManagedSidecarInner {
    state: LeaderState,
    _lease: Option<LeaderLease>,
    heartbeat_tx: Option<std_mpsc::Sender<()>>,
    heartbeat_join: Option<thread::JoinHandle<()>>,
    _bridge: Option<IpcBridge>,
}

struct LeaderLease {
    #[allow(dead_code)]
    lock_path: PathBuf,
    leader_info_path: PathBuf,
    info: LeaderInfo,
    /// 持有的文件锁句柄（unix flock / windows create_new 文件）。
    /// drop 时自动释放锁（unix）或删除文件（windows）。
    _lock_guard: LockGuard,
}

/// 跨平台文件锁守卫。
#[cfg(unix)]
struct LockGuard {
    _file: std::fs::File,
}

#[cfg(unix)]
impl Drop for LockGuard {
    fn drop(&mut self) {
        // File drop 自动释放 flock（close 即解锁），无需显式 unlock。
    }
}

#[cfg(not(unix))]
struct LockGuard {
    lock_path: PathBuf,
}

#[cfg(not(unix))]
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

impl ManagedSidecar {
    pub fn state(&self) -> LeaderState {
        self.inner
            .lock()
            .expect("ManagedSidecar inner lock poisoned")
            .state
            .clone()
    }

    #[allow(dead_code)]
    pub fn is_leader(&self) -> bool {
        matches!(self.state(), LeaderState::Leader)
    }
}

impl Drop for ManagedSidecar {
    fn drop(&mut self) {
        let mut guard = self
            .inner
            .lock()
            .expect("ManagedSidecar inner lock poisoned");
        if let Some(tx) = guard.heartbeat_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = guard.heartbeat_join.take() {
            let _ = join.join();
        }
        // 先释放 bridge（停 IPC server），再让 lease drop 清理 lock/leader.json。
        guard._bridge.take();
        guard._lease.take();
    }
}

impl Drop for LeaderLease {
    fn drop(&mut self) {
        let _ = clear_leader_registration_if_matches(&self.info, &self.leader_info_path);
        // lock 由 LockGuard::drop 释放（unix flock 自动 / windows 删文件），
        // 不再手工 remove_file——避免误删其他进程刚创建的 lock。
    }
}

/// 选举：抢到 Leader 就起 IPC server + service，没抢到就当 Follower 退出。
///
/// `service_factory` 仅在确认成为 Leader 后调用，被淘汰的候选不会构造 service
/// （避免 follower 进程白白打开数据、占用资源）。
pub fn start_or_connect<F>(config: &SidecarConfig, service_factory: F) -> Result<ManagedSidecar>
where
    F: FnOnce() -> Result<Arc<dyn SidecarService>>,
{
    let service = config.service.clone();
    endpoint::ensure_runtime_dir(&service)?;

    for _ in 0..2 {
        if let Some(leader) = read_leader_info(&service)? {
            if is_leader_alive(&service, &config.protocol_version, &leader) {
                // 已有健康 leader，本 sidecar 无需重复启动。
                return Ok(ManagedSidecar {
                    inner: Arc::new(Mutex::new(ManagedSidecarInner {
                        state: LeaderState::Follower { pid: leader.pid },
                        _lease: None,
                        heartbeat_tx: None,
                        heartbeat_join: None,
                        _bridge: None,
                    })),
                });
            }
            clear_stale_registration(&service, &config.protocol_version, &leader)?;
        }

        match try_acquire_leader_lease(&service, &config.protocol_version)? {
            Some(lease) => {
                // 确认成为 Leader 后才构造 service（follower 候选不会走到这里）。
                let service_obj = service_factory()?;
                let bridge = server::spawn_bridge(config, service_obj)?;
                let (heartbeat_tx, heartbeat_join) = spawn_heartbeat(config, lease.info.clone());

                return Ok(ManagedSidecar {
                    inner: Arc::new(Mutex::new(ManagedSidecarInner {
                        state: LeaderState::Leader,
                        _lease: Some(lease),
                        heartbeat_tx: Some(heartbeat_tx),
                        heartbeat_join: Some(heartbeat_join),
                        _bridge: Some(bridge),
                    })),
                });
            }
            None => {
                // flock 被占用（真有进程持有）但 leader.json 还没出现——
                // flock 模式下不删 lock 文件（残留文件不阻塞新进程 open+flock），
                // 短暂等待让持有者写出 leader.json，下一轮循环再判定。
                if let Some(leader) = read_leader_info(&service)? {
                    if is_leader_alive(&service, &config.protocol_version, &leader) {
                        return Ok(ManagedSidecar {
                            inner: Arc::new(Mutex::new(ManagedSidecarInner {
                                state: LeaderState::Follower { pid: leader.pid },
                                _lease: None,
                                heartbeat_tx: None,
                                heartbeat_join: None,
                                _bridge: None,
                            })),
                        });
                    }
                    clear_stale_registration(&service, &config.protocol_version, &leader)?;
                } else {
                    // leader.json 仍不存在，等待持有者写出（flock 仍被占用说明进程存活）。
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }

    bail!("{service} leader 选举失败：未能建立 leader 或确认 follower")
}

fn read_leader_info(service: &str) -> Result<Option<LeaderInfo>> {
    let path = endpoint::leader_info_path(service)?;
    read_leader_info_from_path(&path)
}

fn read_leader_info_from_path(path: &PathBuf) -> Result<Option<LeaderInfo>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    match serde_json::from_str(&content) {
        Ok(info) => Ok(Some(info)),
        Err(_) => {
            tracing::debug!("leader.json 内容损坏，已忽略：{}", path.display());
            Ok(None)
        }
    }
}

/// 判定 leader 是否健康。
///
/// 三重校验：
/// 1. 进程存活 + 心跳未超时
/// 2. endpoint 文件存在（IPC server 崩溃则 IpcServer::drop 删除 endpoint）
/// 3. 协议版本匹配（旧版 leader 视为需替换，让新候选接管）
fn is_leader_alive(service: &str, expected_protocol_version: &str, info: &LeaderInfo) -> bool {
    let heartbeat =
        match chrono::NaiveDateTime::parse_from_str(&info.heartbeat_at, "%Y-%m-%d %H:%M:%S%.f") {
            Ok(value) => value,
            Err(_) => return false,
        };
    let elapsed = chrono::Local::now().naive_local() - heartbeat;
    if elapsed.num_seconds() > HEARTBEAT_TIMEOUT_SECS || !process_is_alive(info.pid) {
        return false;
    }
    // endpoint 文件被 IpcServer::drop 删除说明 IPC server 已停（即使主进程存活）。
    if let Ok(endpoint_path) = endpoint::endpoint_path(service)
        && !endpoint_path.exists()
    {
        return false;
    }
    // 协议版本不匹配（升级场景）：旧 leader 视为不健康，让新候选接管。
    // 空 protocol_version 兼容旧版 leader.json（视为匹配，不阻断）。
    info.protocol_version.is_empty() || info.protocol_version == expected_protocol_version
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // SAFETY: 信号 0 只检查进程是否存在，不会向目标进程发送信号。
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn try_acquire_leader_lease(service: &str, protocol_version: &str) -> Result<Option<LeaderLease>> {
    let lock_path = endpoint::leader_lock_path(service)?;
    let info_path = endpoint::leader_info_path(service)?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 leader 运行目录失败: {}", parent.display()))?;
    }

    // 跨平台文件锁：unix 用 flock 独占锁（崩溃自动释放，进程退出即解锁），
    // windows 回退到 create_new 文件存在性互斥。
    #[cfg(unix)]
    let lock_guard = {
        use std::os::unix::io::AsRawFd;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("打开 leader lock 失败: {}", lock_path.display()))?;
        // SAFETY: flock 对有效 fd 调用，LOCK_EX|LOCK_NB 非阻塞，失败立即返回 EWOULDBLOCK。
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(err)
                .with_context(|| format!("flock leader lock 失败: {}", lock_path.display()));
        }
        LockGuard { _file: file }
    };
    #[cfg(not(unix))]
    let lock_guard = {
        let _file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("创建 leader lock 失败: {}", lock_path.display()));
            }
        };
        LockGuard {
            lock_path: lock_path.clone(),
        }
    };

    let info = LeaderInfo::new(service.to_string(), protocol_version.to_string());
    write_leader_info(&info, &info_path)?;

    Ok(Some(LeaderLease {
        lock_path,
        leader_info_path: info_path,
        info,
        _lock_guard: lock_guard,
    }))
}

fn write_leader_info(info: &LeaderInfo, path: &PathBuf) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 leader 运行目录失败: {}", parent.display()))?;
    }
    let temp_path = leader_info_temp_path(path);
    let content = serde_json::to_string_pretty(info).with_context(|| "序列化 leader 信息失败")?;
    std::fs::write(&temp_path, content)
        .with_context(|| format!("写入临时 leader 信息失败: {}", temp_path.display()))?;
    if let Err(err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err).with_context(|| format!("替换 leader 信息失败: {}", path.display()));
    }
    Ok(())
}

fn leader_info_temp_path(path: &std::path::Path) -> PathBuf {
    let seq = LEADER_INFO_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "leader.json".into());
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), seq))
}

fn update_heartbeat(service: &str, info: &LeaderInfo) -> Result<()> {
    let path = endpoint::leader_info_path(service)?;
    let mut current =
        read_leader_info_from_path(&path)?.ok_or_else(|| anyhow::anyhow!("leader 信息不存在"))?;
    if current.pid != info.pid || current.service != info.service {
        bail!("leader 信息已变更，停止更新心跳");
    }
    current.heartbeat_at = chrono::Local::now().naive_local().to_string();
    write_leader_info(&current, &path)
}

fn spawn_heartbeat(
    config: &SidecarConfig,
    info: LeaderInfo,
) -> (std_mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (tx, rx) = std_mpsc::channel();
    let service = config.service.clone();
    let thread_prefix = config.heartbeat_prefix.clone();
    let join_handle = thread::Builder::new()
        .name(format!("{thread_prefix}-{}", info.service))
        .spawn(move || {
            loop {
                match rx.recv_timeout(HEARTBEAT_INTERVAL) {
                    Ok(()) => break,
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(err) = update_heartbeat(&service, &info) {
                            tracing::debug!("{service} leader 心跳停止: {}", err);
                            break;
                        }
                    }
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("创建 leader 心跳线程失败");
    (tx, join_handle)
}

fn clear_stale_registration(
    service: &str,
    expected_protocol_version: &str,
    info: &LeaderInfo,
) -> Result<()> {
    if is_leader_alive(service, expected_protocol_version, info) {
        return Ok(());
    }
    let info_path = endpoint::leader_info_path(service)?;
    clear_leader_registration_if_matches(info, &info_path)?;
    // flock 模式下删 lock 文件是 best-effort（残留文件不阻塞新候选 open+flock）。
    let lock_path = endpoint::leader_lock_path(service)?;
    let _ = std::fs::remove_file(lock_path);
    Ok(())
}

fn clear_leader_registration_if_matches(info: &LeaderInfo, path: &PathBuf) -> Result<()> {
    if let Some(current) = read_leader_info_from_path(path)?
        && current.pid == info.pid
        && current.service == info.service
    {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
