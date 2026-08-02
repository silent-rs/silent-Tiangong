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
}

impl LeaderInfo {
    fn new(service: String) -> Self {
        let now = chrono::Local::now().naive_local().to_string();
        Self {
            pid: std::process::id(),
            service,
            started_at: now.clone(),
            heartbeat_at: now,
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
    lock_path: PathBuf,
    leader_info_path: PathBuf,
    info: LeaderInfo,
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
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// 选举：抢到 Leader 就起 IPC server + service，没抢到就当 Follower 退出。
pub fn start_or_connect(
    config: &SidecarConfig,
    service_obj: Arc<dyn SidecarService>,
) -> Result<ManagedSidecar> {
    let service = config.service.clone();
    endpoint::ensure_runtime_dir(&service)?;

    for _ in 0..2 {
        if let Some(leader) = read_leader_info(&service)? {
            if is_leader_alive(&leader) {
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
            clear_stale_registration(&service, &leader)?;
        }

        match try_acquire_leader_lease(&service)? {
            Some(lease) => {
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
                // lock 被占用但 leader.json 还没出现，或 leader.json 出现了——重试。
                if let Some(leader) = read_leader_info(&service)? {
                    if is_leader_alive(&leader) {
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
                    clear_stale_registration(&service, &leader)?;
                } else {
                    let lock_path = endpoint::leader_lock_path(&service)?;
                    if lock_path.exists() {
                        let _ = std::fs::remove_file(lock_path);
                    }
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

fn is_leader_alive(info: &LeaderInfo) -> bool {
    let heartbeat =
        match chrono::NaiveDateTime::parse_from_str(&info.heartbeat_at, "%Y-%m-%d %H:%M:%S%.f") {
            Ok(value) => value,
            Err(_) => return false,
        };
    let elapsed = chrono::Local::now().naive_local() - heartbeat;
    elapsed.num_seconds() <= HEARTBEAT_TIMEOUT_SECS && process_is_alive(info.pid)
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

fn try_acquire_leader_lease(service: &str) -> Result<Option<LeaderLease>> {
    let lock_path = endpoint::leader_lock_path(service)?;
    let info_path = endpoint::leader_info_path(service)?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 leader 运行目录失败: {}", parent.display()))?;
    }
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

    let info = LeaderInfo::new(service.to_string());
    if let Err(err) = write_leader_info(&info, &info_path) {
        let _ = std::fs::remove_file(&lock_path);
        return Err(err);
    }

    Ok(Some(LeaderLease {
        lock_path,
        leader_info_path: info_path,
        info,
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

fn clear_stale_registration(service: &str, info: &LeaderInfo) -> Result<()> {
    if is_leader_alive(info) {
        return Ok(());
    }
    let info_path = endpoint::leader_info_path(service)?;
    clear_leader_registration_if_matches(info, &info_path)?;
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
