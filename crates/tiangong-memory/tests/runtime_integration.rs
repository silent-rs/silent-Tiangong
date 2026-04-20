use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tiangong_memory::{
    Episode, EpisodeOutcome, RecallAnchors, TurnResult, load_injection_sync, start,
    workspace_id_from_path,
};

struct EnvGuard {
    prev_home: Option<std::ffi::OsString>,
    prev_userprofile: Option<std::ffi::OsString>,
    prev_cwd: PathBuf,
}

impl EnvGuard {
    fn enter(home: &Path, cwd: &Path) -> Self {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        let prev_cwd = std::env::current_dir().expect("读取当前工作目录失败");

        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        std::env::set_current_dir(cwd).expect("切换当前工作目录失败");

        Self {
            prev_home,
            prev_userprofile,
            prev_cwd,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev_cwd);
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

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("创建目录失败");
    }
    fs::write(path, content).expect("写入文件失败");
}

fn setup_workspace() -> (TempDir, TempDir, PathBuf, String) {
    let home = TempDir::new().expect("创建 fake home 失败");
    let workspace = TempDir::new().expect("创建 workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    (home, workspace, workspace_path, workspace_id)
}

async fn wait_for_recall_hit(
    handle: &tiangong_memory::MemoryHandle,
    query: &str,
) -> Vec<tiangong_memory::RecallHit> {
    for _ in 0..20 {
        let hits = handle
            .recall(
                RecallAnchors {
                    query: query.to_string(),
                    keywords: Vec::new(),
                    strategy: None,
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

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn runtime_loads_profile_workspace_and_session_injections() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    write_file(
        &home.path().join(".tiangong/memory/profile/agent.md"),
        "profile-memory",
    );
    write_file(
        &home.path().join(format!(
            ".tiangong/memory/workspaces/{workspace_id}/agent.md"
        )),
        "workspace-memory",
    );
    write_file(
        &home
            .path()
            .join(".tiangong/memory/sessions/session-a/agent.md"),
        "session-memory",
    );

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");
    let loaded = handle
        .load_injection("session-a", Some(&workspace_id))
        .await;

    assert_eq!(
        loaded,
        vec![
            "profile-memory".to_string(),
            "workspace-memory".to_string(),
            "session-memory".to_string(),
        ]
    );

    let sync_loaded = load_injection_sync("session-a", Some(&workspace_id));
    assert_eq!(loaded, sync_loaded);

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn runtime_can_write_episode_and_recall_without_core() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");
    handle.write_episode(
        Episode::new(
            "session-b".to_string(),
            "fix login timeout".to_string(),
            "fix login timeout and retry flow".to_string(),
            EpisodeOutcome::Success,
            vec![
                "login".to_string(),
                "timeout".to_string(),
                "retry".to_string(),
            ],
            vec!["http_request".to_string()],
            0.8,
        ),
        Some(workspace_id.clone()),
    );

    let hits = wait_for_recall_hit(&handle, "login timeout").await;
    assert!(
        !hits.is_empty(),
        "写入 Episode 后应能通过 recall 命中 Tantivy 索引"
    );
    assert!(
        hits.iter().any(|hit| {
            hit.title.contains("login timeout") || hit.summary.contains("login timeout")
        }),
        "命中结果应包含已写入 Episode 的标题或摘要"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn micro_rumination_writes_episode_with_explicit_workspace_context() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start(None).expect("启动 memory 失败");
    handle.run_micro_rumination(TurnResult {
        session_id: "session-c".to_string(),
        turn_id: "turn-c".to_string(),
        had_tool_calls: true,
        summary: "complete database migration and fix schema mismatch".to_string(),
        workspace_id: Some(workspace_id.clone()),
    });

    let hits = wait_for_recall_hit(&handle, "database migration").await;
    assert!(
        !hits.is_empty(),
        "micro rumination 写入后应能通过 recall 命中 Episode"
    );

    handle.shutdown().await;
}
