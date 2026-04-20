//! Leader 选举与进程注册（Phase B MVP）
//!
//! 当前实现提供一个可运行的最小闭环：
//! - 通过 `leader.lock` 原子创建实现单 Leader 互斥
//! - 通过 `leader.json` 记录 leader 服务信息与心跳
//! - Follower 优先连接已有 leader；若 leader 不可用则参与选举
//! - 若检测到不同 workspace 的 leader 存在，则显式报错，避免串连

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::actor::start_memory;
use crate::handle::MemoryHandle;
use crate::ipc::{IpcBridge, spawn_memory_bridge};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT_SECS: i64 = 10;

/// 进程类型（用于 Leader 选举，区分 GUI/CLI/Server）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessType {
    Gui,
    Cli,
    Server,
}

/// Leader 状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderState {
    /// 本进程是 Leader（持有 lock 文件并维护心跳）
    Leader,
    /// 本进程是 Follower（通过 IPC 访问 Leader）
    Follower { pid: u32 },
}

/// leader.json 中的注册信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderInfo {
    pub pid: u32,
    pub process_type: ProcessType,
    pub workspace_id: Option<String>,
    pub service: String,
    pub started_at: String,
    pub heartbeat_at: String,
}

/// 带运行时守卫的 Memory 句柄。
pub struct ManagedMemory {
    handle: MemoryHandle,
    state: LeaderState,
    _bridge: Option<IpcBridge>,
    _lease: Option<LeaderLease>,
    heartbeat_tx: Option<std_mpsc::Sender<()>>,
    heartbeat_join: Option<thread::JoinHandle<()>>,
}

struct LeaderLease {
    lock_path: PathBuf,
    leader_info_path: PathBuf,
    info: LeaderInfo,
}

impl ManagedMemory {
    pub fn handle(&self) -> MemoryHandle {
        self.handle.clone()
    }

    pub fn state(&self) -> &LeaderState {
        &self.state
    }

    pub fn is_leader(&self) -> bool {
        matches!(self.state, LeaderState::Leader)
    }
}

impl Drop for ManagedMemory {
    fn drop(&mut self) {
        if let Some(tx) = self.heartbeat_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join_handle) = self.heartbeat_join.take() {
            let _ = join_handle.join();
        }
    }
}

impl Drop for LeaderLease {
    fn drop(&mut self) {
        let _ = clear_leader_registration_if_matches(&self.info, &self.leader_info_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// 生成当前 workspace 对应的 memory IPC service 名称。
pub fn memory_service_name(workspace_id: Option<&str>) -> String {
    match workspace_id {
        Some(workspace_id) if !workspace_id.is_empty() => format!("memory-{workspace_id}"),
        _ => "memory-default".to_string(),
    }
}

/// 获取 Leader 锁文件路径
pub fn leader_lock_path() -> PathBuf {
    memory_base_dir().join("leader.lock")
}

/// 获取 Leader 信息文件路径
pub fn leader_info_path() -> PathBuf {
    memory_base_dir().join("leader.json")
}

/// 读取当前 leader 信息。
pub fn read_leader_info() -> Result<Option<LeaderInfo>> {
    let path = leader_info_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("读取 leader 信息失败: {}", path.display()))?;
    let info = serde_json::from_str(&content)
        .with_context(|| format!("解析 leader 信息失败: {}", path.display()))?;
    Ok(Some(info))
}

/// 判断给定 leader 是否仍然健康。
pub fn is_leader_alive(info: &LeaderInfo) -> bool {
    let heartbeat =
        match chrono::NaiveDateTime::parse_from_str(&info.heartbeat_at, "%Y-%m-%d %H:%M:%S%.f") {
            Ok(value) => value,
            Err(_) => return false,
        };
    let elapsed = chrono::Local::now().naive_local() - heartbeat;
    elapsed.num_seconds() <= HEARTBEAT_TIMEOUT_SECS
}

/// 选举或连接到现有 leader。
pub async fn start_or_connect(
    workspace_id: Option<String>,
    process_type: ProcessType,
) -> Result<ManagedMemory> {
    let service = memory_service_name(workspace_id.as_deref());
    start_or_connect_with_service(workspace_id, process_type, service).await
}

/// 使用自定义 service 名称执行选举或连接。
pub async fn start_or_connect_with_service(
    workspace_id: Option<String>,
    process_type: ProcessType,
    service: impl Into<String>,
) -> Result<ManagedMemory> {
    let service = service.into();
    ensure_memory_base_dir()?;

    for _ in 0..2 {
        if let Some(leader) = read_leader_info()? {
            ensure_workspace_compatible(&leader, workspace_id.as_deref())?;
            if is_leader_alive(&leader) {
                match MemoryHandle::connect_tcp(&leader.service).await {
                    Ok(handle) => {
                        return Ok(ManagedMemory {
                            handle,
                            state: LeaderState::Follower { pid: leader.pid },
                            _bridge: None,
                            _lease: None,
                            heartbeat_tx: None,
                            heartbeat_join: None,
                        });
                    }
                    Err(err) => {
                        tracing::warn!("连接现有 Memory leader 失败，尝试重新选举: {}", err);
                    }
                }
            } else {
                clear_stale_registration(&leader)?;
            }
        }

        match try_acquire_leader_lease(workspace_id.clone(), process_type.clone(), service.clone())?
        {
            Some(lease) => {
                let handle = start_memory(workspace_id.clone())?;
                let bridge = spawn_memory_bridge(service.clone(), handle.clone())?;
                let (heartbeat_tx, heartbeat_join) = spawn_heartbeat(lease.info.clone());

                return Ok(ManagedMemory {
                    handle,
                    state: LeaderState::Leader,
                    _bridge: Some(bridge),
                    _lease: Some(lease),
                    heartbeat_tx: Some(heartbeat_tx),
                    heartbeat_join: Some(heartbeat_join),
                });
            }
            None => {
                if let Some(leader) = read_leader_info()? {
                    ensure_workspace_compatible(&leader, workspace_id.as_deref())?;
                    if is_leader_alive(&leader) {
                        let handle = MemoryHandle::connect_tcp(&leader.service)
                            .await
                            .with_context(|| "连接选举完成后的 Memory leader 失败")?;
                        return Ok(ManagedMemory {
                            handle,
                            state: LeaderState::Follower { pid: leader.pid },
                            _bridge: None,
                            _lease: None,
                            heartbeat_tx: None,
                            heartbeat_join: None,
                        });
                    }
                    clear_stale_registration(&leader)?;
                } else if leader_lock_path().exists() {
                    let _ = std::fs::remove_file(leader_lock_path());
                }
            }
        }
    }

    bail!("Memory leader 选举失败：未能建立 leader 或连接 follower")
}

fn try_acquire_leader_lease(
    workspace_id: Option<String>,
    process_type: ProcessType,
    service: String,
) -> Result<Option<LeaderLease>> {
    let lock_path = leader_lock_path();
    let leader_info_path = leader_info_path();
    let opened = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path);

    let _file = match opened {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("创建 leader lock 失败: {}", lock_path.display()));
        }
    };

    let now = chrono::Local::now().naive_local().to_string();
    let info = LeaderInfo {
        pid: std::process::id(),
        process_type,
        workspace_id,
        service,
        started_at: now.clone(),
        heartbeat_at: now,
    };
    write_leader_info(&info, &leader_info_path)?;

    Ok(Some(LeaderLease {
        lock_path,
        leader_info_path,
        info,
    }))
}

fn write_leader_info(info: &LeaderInfo, path: &PathBuf) -> Result<()> {
    let temp_path = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(info).with_context(|| "序列化 leader 信息失败")?;
    std::fs::write(&temp_path, content)
        .with_context(|| format!("写入临时 leader 信息失败: {}", temp_path.display()))?;
    std::fs::rename(&temp_path, path)
        .with_context(|| format!("替换 leader 信息失败: {}", path.display()))
}

fn update_heartbeat(info: &LeaderInfo) -> Result<()> {
    let path = leader_info_path();
    let mut current = match read_leader_info()? {
        Some(current) => current,
        None => bail!("leader 信息不存在"),
    };
    if current.pid != info.pid || current.service != info.service {
        bail!("leader 信息已变更，停止更新心跳");
    }
    current.heartbeat_at = chrono::Local::now().naive_local().to_string();
    write_leader_info(&current, &path)
}

fn spawn_heartbeat(info: LeaderInfo) -> (std_mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (tx, rx) = std_mpsc::channel();
    let join_handle = thread::Builder::new()
        .name(format!("memory-heartbeat-{}", info.service))
        .spawn(move || {
            loop {
                match rx.recv_timeout(HEARTBEAT_INTERVAL) {
                    Ok(()) => break,
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(err) = update_heartbeat(&info) {
                            tracing::debug!("Memory leader 心跳停止: {}", err);
                            break;
                        }
                    }
                    Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("创建 Memory leader 心跳线程失败");
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
    let Some(current) = read_leader_info()? else {
        return Ok(());
    };
    if current.pid == info.pid && current.service == info.service {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn ensure_workspace_compatible(leader: &LeaderInfo, workspace_id: Option<&str>) -> Result<()> {
    if leader.workspace_id.as_deref() == workspace_id {
        return Ok(());
    }
    Err(anyhow!(
        "当前 Memory leader 绑定的 workspace 与请求不一致：leader={:?} request={:?}",
        leader.workspace_id,
        workspace_id
    ))
}

fn ensure_memory_base_dir() -> Result<()> {
    let base = memory_base_dir();
    std::fs::create_dir_all(&base)
        .with_context(|| format!("创建 memory 运行目录失败: {}", base.display()))
}

fn memory_base_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
}

fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_userprofile: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn enter(home: &std::path::Path) -> Self {
            let prev_home = std::env::var_os("HOME");
            let prev_userprofile = std::env::var_os("USERPROFILE");
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("USERPROFILE", home);
            }
            Self {
                prev_home,
                prev_userprofile,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_userprofile {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn leader_info_roundtrip_and_stale_detection_work() {
        let home = TempDir::new().expect("创建 fake home 失败");
        let _env = EnvGuard::enter(home.path());
        ensure_memory_base_dir().expect("创建 memory 目录失败");

        let stale_time =
            (chrono::Local::now().naive_local() - chrono::TimeDelta::seconds(30)).to_string();
        let info = LeaderInfo {
            pid: 42,
            process_type: ProcessType::Cli,
            workspace_id: Some("ws-test".to_string()),
            service: "memory-ws-test".to_string(),
            started_at: stale_time.clone(),
            heartbeat_at: stale_time,
        };
        write_leader_info(&info, &leader_info_path()).expect("写入 leader 信息失败");

        let loaded = read_leader_info()
            .expect("读取 leader 信息失败")
            .expect("leader 信息不存在");
        assert_eq!(loaded.service, "memory-ws-test");
        assert!(!is_leader_alive(&loaded), "过期心跳应判定为不存活");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn start_or_connect_builds_leader_then_follower() {
        let home = TempDir::new().expect("创建 fake home 失败");
        let _env = EnvGuard::enter(home.path());

        let leader = start_or_connect(Some("ws-election".to_string()), ProcessType::Cli)
            .await
            .expect("启动 leader 失败");
        assert!(leader.is_leader(), "首个进程应成为 leader");

        let follower = start_or_connect(Some("ws-election".to_string()), ProcessType::Server)
            .await
            .expect("连接 follower 失败");
        assert!(
            matches!(follower.state(), LeaderState::Follower { .. }),
            "第二个调用方应作为 follower 连接 leader"
        );

        follower.handle().write_episode(
            crate::types::Episode::new(
                "session-election".to_string(),
                "elect leader".to_string(),
                "elect leader through tcp follower".to_string(),
                crate::types::EpisodeOutcome::Success,
                vec!["leader".to_string(), "tcp".to_string()],
                vec!["memory_election".to_string()],
                0.8,
            ),
            Some("ws-election".to_string()),
        );

        let follower_handle = follower.handle();
        let hits = wait_for_recall_hit(&follower_handle, "elect leader").await;
        assert!(
            hits.iter().any(|hit| hit.title.contains("elect leader")),
            "follower 应能通过 leader 的远端句柄完成写入与召回"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn start_or_connect_rejects_workspace_mismatch() {
        let home = TempDir::new().expect("创建 fake home 失败");
        let _env = EnvGuard::enter(home.path());

        let _leader = start_or_connect(Some("ws-a".to_string()), ProcessType::Cli)
            .await
            .expect("启动 leader 失败");

        let err = match start_or_connect(Some("ws-b".to_string()), ProcessType::Server).await {
            Ok(_) => panic!("不同 workspace 不应复用同一个 leader"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("workspace"),
            "错误信息应明确指出 workspace 不一致"
        );
    }

    async fn wait_for_recall_hit(
        handle: &MemoryHandle,
        query: &str,
    ) -> Vec<crate::types::RecallHit> {
        for _ in 0..20 {
            let hits = handle
                .recall(
                    crate::types::RecallAnchors {
                        query: query.to_string(),
                        keywords: Vec::new(),
                    },
                    5,
                )
                .await;
            if !hits.is_empty() {
                return hits;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Vec::new()
    }
}
