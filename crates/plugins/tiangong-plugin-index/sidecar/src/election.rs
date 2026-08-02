//! Leader 选举与进程注册。
//!
//! 对齐 Memory / MCP sidecar 的选举机制：
//! - `leader.lock` 原子创建实现单 Leader 互斥
//! - `leader.json` 记录 leader 服务信息与心跳
//! - 已有健康 leader 时本 sidecar 优雅退出（不重复运行）
//!
//! Index sidecar 由运行时按 `plugin.json` 启动，运行时通过 endpoint 文件连接 winner。

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::service::IndexService;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT_SECS: i64 = 10;
static LEADER_INFO_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// 带运行时守卫的 Index 服务。
pub struct ManagedIndex {
    inner: Arc<Mutex<ManagedIndexInner>>,
}

struct ManagedIndexInner {
    state: LeaderState,
    _lease: Option<LeaderLease>,
    heartbeat_tx: Option<std_mpsc::Sender<()>>,
    heartbeat_join: Option<thread::JoinHandle<()>>,
    _bridge: Option<crate::ipc::IpcBridge>,
}

struct LeaderLease {
    lock_path: PathBuf,
    leader_info_path: PathBuf,
    info: LeaderInfo,
}

impl ManagedIndex {
    pub fn state(&self) -> LeaderState {
        self.inner
            .lock()
            .expect("ManagedIndex inner lock poisoned")
            .state
            .clone()
    }

    #[allow(dead_code)]
    pub fn is_leader(&self) -> bool {
        matches!(self.state(), LeaderState::Leader)
    }
}

impl Drop for ManagedIndex {
    fn drop(&mut self) {
        let mut guard = self.inner.lock().expect("ManagedIndex inner lock poisoned");
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

/// 选举：抢到 Leader 就起 IPC server + IndexService，没抢到就当 Follower 退出。
pub async fn start_or_connect() -> Result<ManagedIndex> {
    let service = index_service_name();
    ensure_index_runtime_dir()?;

    for _ in 0..2 {
        if let Some(leader) = read_leader_info()? {
            if is_leader_alive(&leader) {
                // 已有健康 leader，本 sidecar 无需重复启动。
                return Ok(ManagedIndex {
                    inner: Arc::new(Mutex::new(ManagedIndexInner {
                        state: LeaderState::Follower { pid: leader.pid },
                        _lease: None,
                        heartbeat_tx: None,
                        heartbeat_join: None,
                        _bridge: None,
                    })),
                });
            }
            clear_stale_registration(&leader)?;
        }

        match try_acquire_leader_lease(service.clone())? {
            Some(lease) => {
                let service_obj = IndexService::new()?;
                let bridge = crate::ipc::spawn_index_bridge(service.clone(), service_obj)?;
                let (heartbeat_tx, heartbeat_join) = spawn_heartbeat(lease.info.clone());

                return Ok(ManagedIndex {
                    inner: Arc::new(Mutex::new(ManagedIndexInner {
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
                if let Some(leader) = read_leader_info()? {
                    if is_leader_alive(&leader) {
                        return Ok(ManagedIndex {
                            inner: Arc::new(Mutex::new(ManagedIndexInner {
                                state: LeaderState::Follower { pid: leader.pid },
                                _lease: None,
                                heartbeat_tx: None,
                                heartbeat_join: None,
                                _bridge: None,
                            })),
                        });
                    }
                    clear_stale_registration(&leader)?;
                } else {
                    let lock_path = leader_lock_path();
                    if lock_path.exists() {
                        let _ = std::fs::remove_file(lock_path);
                    }
                }
            }
        }
    }

    bail!("Index leader 选举失败：未能建立 leader 或确认 follower")
}

fn index_service_name() -> String {
    "index".to_string()
}

fn leader_lock_path() -> PathBuf {
    index_runtime_dir().join("leader.lock")
}

fn leader_info_path() -> PathBuf {
    index_runtime_dir().join("leader.json")
}

fn read_leader_info() -> Result<Option<LeaderInfo>> {
    read_leader_info_from_path(&leader_info_path())
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

fn try_acquire_leader_lease(service: String) -> Result<Option<LeaderLease>> {
    let lock_path = leader_lock_path();
    let info_path = leader_info_path();
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

    let info = LeaderInfo::new(service);
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

fn update_heartbeat(info: &LeaderInfo) -> Result<()> {
    let path = leader_info_path();
    let mut current =
        read_leader_info_from_path(&path)?.ok_or_else(|| anyhow::anyhow!("leader 信息不存在"))?;
    if current.pid != info.pid || current.service != info.service {
        bail!("leader 信息已变更，停止更新心跳");
    }
    current.heartbeat_at = chrono::Local::now().naive_local().to_string();
    write_leader_info(&current, &path)
}

fn spawn_heartbeat(info: LeaderInfo) -> (std_mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (tx, rx) = std_mpsc::channel();
    let join_handle = thread::Builder::new()
        .name(format!("index-heartbeat-{}", info.service))
        .spawn(move || {
            loop {
                match rx.recv_timeout(HEARTBEAT_INTERVAL) {
                    Ok(()) => break,
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(err) = update_heartbeat(&info) {
                            tracing::debug!("Index leader 心跳停止: {}", err);
                            break;
                        }
                    }
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("创建 Index leader 心跳线程失败");
    (tx, join_handle)
}

fn clear_stale_registration(info: &LeaderInfo) -> Result<()> {
    if is_leader_alive(info) {
        return Ok(());
    }
    clear_leader_registration_if_matches(info, &leader_info_path())?;
    let _ = std::fs::remove_file(leader_lock_path());
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

fn ensure_index_runtime_dir() -> Result<()> {
    let dir = index_runtime_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建 index 运行目录失败: {}", dir.display()))
}

/// Index 运行时目录（endpoint.json / leader.json / leader.lock 所在）。
///
/// 优先使用运行时注入的 `TIANGONG_PLUGIN_DATA_DIR`（与 plugin-runtime 的 sidecar
/// 配置对齐），否则回退 `~/.tiangong/index/runtime`。
fn index_runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TIANGONG_PLUGIN_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("runtime");
    }
    if let Some(dir) = std::env::var_os("TIANGONG_PLUGIN_ENDPOINT").filter(|v| !v.is_empty())
        && let Some(parent) = PathBuf::from(dir).parent()
    {
        return parent.to_path_buf();
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("index")
        .join("runtime")
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
