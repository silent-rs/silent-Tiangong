use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tiangong_memory::{
    Episode, EpisodeOutcome, LeaderState, ManagedMemory, ProcessType, RecallAnchors,
    read_leader_info, start_or_connect,
};

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

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn follower_continues_memory_after_leader_disappears() {
    let home = TempDir::new().expect("创建 fake home 失败");
    let _env = EnvGuard::enter(home.path());
    let workspace_id = "ws-auto-continuation".to_string();

    log_step(format!(
        "测试开始：home={} workspace={workspace_id}",
        home.path().display()
    ));

    let leader = start_or_connect(Some(workspace_id.clone()), ProcessType::Cli)
        .await
        .expect("启动 leader 失败");
    log_state("leader 启动后", &leader);
    log_leader_info("leader 启动后");
    assert!(leader.is_leader(), "首个 memory 实例应成为 leader");

    let follower = start_or_connect(Some(workspace_id.clone()), ProcessType::Server)
        .await
        .expect("启动 follower 失败");
    log_state("follower 启动后", &follower);
    log_leader_info("follower 连接后");
    assert!(
        matches!(follower.state(), LeaderState::Follower { .. }),
        "第二个 memory 实例应通过 TCP 连接 leader"
    );

    log_step("通过 follower 写入接替前 Episode");
    follower.handle().write_episode(
        make_episode(
            "session-before-failover",
            "persist before failover",
            "persist before failover through follower",
            vec!["before".to_string(), "failover".to_string()],
        ),
        Some(workspace_id.clone()),
    );
    assert_recall_contains(&follower, "before failover", "persist before failover").await;

    log_step("drop leader，模拟 leader 进程退出");
    drop(leader);
    wait_until_leader(&follower).await;
    log_state("follower 自动接替后", &follower);
    log_leader_info("follower 自动接替后");

    log_step("验证接替前写入的 Episode 仍可召回");
    assert_recall_contains(&follower, "before failover", "persist before failover").await;

    log_step("通过晋升后的 follower 写入接替后 Episode");
    follower.handle().write_episode(
        make_episode(
            "session-after-failover",
            "write after failover",
            "write after failover using promoted follower",
            vec!["after".to_string(), "failover".to_string()],
        ),
        Some(workspace_id),
    );
    assert_recall_contains(&follower, "after failover", "write after failover").await;
    log_step("测试完成：自动接续链路验证通过");
}

fn make_episode(session_id: &str, title: &str, summary: &str, keywords: Vec<String>) -> Episode {
    Episode::new(
        session_id.to_string(),
        title.to_string(),
        summary.to_string(),
        EpisodeOutcome::Success,
        keywords,
        vec!["memory_failover_integration".to_string()],
        0.9,
    )
}

async fn wait_until_leader(memory: &ManagedMemory) {
    for attempt in 0..40 {
        if memory.is_leader() {
            log_step(format!(
                "follower 已在第 {} 次检查时晋升为 leader",
                attempt + 1
            ));
            return;
        }
        if attempt % 5 == 0 {
            log_step(format!(
                "等待 follower 自动接替：attempt={} state={:?}",
                attempt + 1,
                memory.state()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("follower 未在预期时间内自动接替为 leader");
}

async fn assert_recall_contains(memory: &ManagedMemory, query: &str, expected_title: &str) {
    for attempt in 0..30 {
        let hits = memory
            .handle()
            .recall(
                RecallAnchors {
                    query: query.to_string(),
                    keywords: Vec::new(),
                },
                5,
            )
            .await;
        if attempt == 0 || !hits.is_empty() {
            let titles = hits
                .iter()
                .map(|hit| format!("{}({:.3})", hit.title, hit.score))
                .collect::<Vec<_>>()
                .join(", ");
            log_step(format!(
                "召回检查：query={query} attempt={} hits=[{}]",
                attempt + 1,
                titles
            ));
        }
        if hits.iter().any(|hit| hit.title.contains(expected_title)) {
            log_step(format!("召回命中预期记忆：title={expected_title}"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未召回预期记忆：query={query}, title={expected_title}");
}

fn log_step(message: impl AsRef<str>) {
    eprintln!("[memory-failover-it] {}", message.as_ref());
}

fn log_state(label: &str, memory: &ManagedMemory) {
    log_step(format!("{label}: state={:?}", memory.state()));
}

fn log_leader_info(label: &str) {
    match read_leader_info() {
        Ok(Some(info)) => log_step(format!(
            "{label}: leader pid={} process={:?} workspace={:?} service={} heartbeat={}",
            info.pid, info.process_type, info.workspace_id, info.service, info.heartbeat_at
        )),
        Ok(None) => log_step(format!("{label}: leader 信息不存在")),
        Err(err) => log_step(format!("{label}: 读取 leader 信息失败: {err}")),
    }
}
