use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tiangong_memory::{
    Episode, EpisodeOutcome, MemoryRecallRequest, RecallAnchors, TurnArtifact, TurnArtifactKind,
    TurnResult, load_injection_sync, start, workspace_id_from_path,
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
async fn runtime_can_expand_recalled_episode_with_depth2() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");
    handle.write_episode(
        Episode::new(
            "session-depth2".to_string(),
            "depth2 expansion smoke".to_string(),
            "depth2 full content should include the original episode payload".to_string(),
            EpisodeOutcome::Success,
            vec![
                "depth2".to_string(),
                "expansion".to_string(),
                "payload".to_string(),
            ],
            vec!["memory_depth2_tool".to_string()],
            0.85,
        ),
        Some(workspace_id.clone()),
    );

    let hits = wait_for_recall_hit(&handle, "depth2 expansion").await;
    assert!(!hits.is_empty(), "测试前应先召回刚写入的 Episode");

    let expanded = handle
        .load_depth2(vec![hits[0].node_id.clone(), "missing-node".to_string()])
        .await;
    assert_eq!(expanded.len(), 1, "Depth2 应跳过不存在的节点");
    assert_eq!(expanded[0].node_id, hits[0].node_id);
    assert!(
        expanded[0]
            .full_content
            .contains("depth2 full content should include"),
        "Depth2 应返回 Episode 的完整序列化内容"
    );
    assert!(
        expanded[0].full_content.contains("memory_depth2_tool"),
        "Depth2 完整内容应包含 tool_calls 等摘要外信息"
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
        ..TurnResult::default()
    });

    let hits = wait_for_recall_hit(&handle, "database migration").await;
    assert!(
        !hits.is_empty(),
        "micro rumination 写入后应能通过 recall 命中 Episode"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn memory_recall_levels_cover_injection_depth1_depth2_and_context_summary() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    eprintln!("[memory-levels] fake_home={}", home.path().display());
    eprintln!(
        "[memory-levels] workspace_path={}",
        workspace_path.display()
    );
    eprintln!("[memory-levels] workspace_id={workspace_id}");

    write_file(
        &home.path().join(".tiangong/memory/profile/agent.md"),
        "profile recall level",
    );
    write_file(
        &home.path().join(format!(
            ".tiangong/memory/workspaces/{workspace_id}/agent.md"
        )),
        "workspace recall level",
    );
    write_file(
        &home
            .path()
            .join(".tiangong/memory/sessions/session-levels/agent.md"),
        "session recall level",
    );

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");

    let injection = handle
        .load_injection("session-levels", Some(&workspace_id))
        .await;
    eprintln!(
        "[memory-levels] injection_count={} items={:#?}",
        injection.len(),
        injection
    );
    assert_eq!(
        injection,
        vec![
            "profile recall level".to_string(),
            "workspace recall level".to_string(),
            "session recall level".to_string(),
        ],
        "Injection 层应按 profile -> workspace -> session 顺序加载"
    );

    let image_url = "https://example.invalid/media/raven-drinking-water.png";
    let file_path = "/tmp/tiangong/raven-drinking-water.png";
    handle.run_micro_rumination(TurnResult {
        session_id: "session-levels".to_string(),
        turn_id: "turn-raven-image".to_string(),
        had_tool_calls: true,
        user_input: "generate raven drinking water image".to_string(),
        summary: "generated raven drinking water image with seedream model".to_string(),
        tool_calls: vec!["generate_image".to_string(), "write_file".to_string()],
        artifacts: vec![
            TurnArtifact {
                kind: TurnArtifactKind::Media,
                tool_name: Some("generate_image".to_string()),
                title: Some("raven drinking water image".to_string()),
                url: Some(image_url.to_string()),
                path: None,
                summary: Some("final image artifact".to_string()),
            },
            TurnArtifact {
                kind: TurnArtifactKind::File,
                tool_name: Some("write_file".to_string()),
                title: Some("local image file".to_string()),
                url: None,
                path: Some(file_path.to_string()),
                summary: Some("saved generated image file".to_string()),
            },
        ],
        workspace_id: Some(workspace_id.clone()),
    });
    eprintln!(
        "[memory-levels] micro_rumination submitted media_url={image_url} file_path={file_path}"
    );
    handle.write_episode(
        Episode::new(
            "session-levels".to_string(),
            "duplicate raven image artifact".to_string(),
            format!("duplicate raven image memory pointing at {image_url}"),
            EpisodeOutcome::Success,
            vec![
                "raven".to_string(),
                "image".to_string(),
                "seedream".to_string(),
            ],
            vec!["generate_image".to_string()],
            0.7,
        ),
        Some(workspace_id.clone()),
    );
    eprintln!("[memory-levels] duplicate episode submitted for URL dedupe coverage");

    let hits = wait_for_recall_hit(&handle, "raven image seedream").await;
    eprintln!("[memory-levels] depth1_hit_count={}", hits.len());
    for (idx, hit) in hits.iter().enumerate() {
        eprintln!(
            "[memory-levels] depth1_hit[{idx}] node_id={} score={:.3} kind={:?} title={} summary={}",
            hit.node_id, hit.score, hit.kind, hit.title, hit.summary
        );
    }
    assert!(
        hits.iter()
            .any(|hit| hit.summary.contains("raven drinking water image")),
        "Depth1 粗召回应返回 Episode 摘要"
    );

    let expanded = handle
        .load_depth2(hits.iter().map(|hit| hit.node_id.clone()).collect())
        .await;
    eprintln!("[memory-levels] depth2_item_count={}", expanded.len());
    for (idx, item) in expanded.iter().enumerate() {
        eprintln!(
            "[memory-levels] depth2_item[{idx}] node_id={} contains_image_url={} contains_file_path={} contains_generate_image={} preview={}",
            item.node_id,
            item.full_content.contains(image_url),
            item.full_content.contains(file_path),
            item.full_content.contains("generate_image"),
            compact_for_log(&item.full_content, 700)
        );
    }
    let expanded_text = expanded
        .iter()
        .map(|item| item.full_content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        expanded_text.contains(image_url),
        "Depth2 展开应包含结构化媒体 URL"
    );
    assert!(
        expanded_text.contains(file_path),
        "Depth2 展开应包含结构化文件产物路径"
    );
    assert!(
        expanded_text.contains("generate_image"),
        "Depth2 展开应保留工具调用信息"
    );

    let contextual = handle
        .recall_context(MemoryRecallRequest {
            query: "raven image seedream".to_string(),
            reason: Some("用户要求继续使用之前生成的图片".to_string()),
            expected: vec!["media".to_string(), "file".to_string()],
            context: vec![
                "user: raven image seedream".to_string(),
                "assistant: 当前上下文只包含用户请求，不包含图片 URL".to_string(),
            ],
            limit: 5,
        })
        .await;
    eprintln!(
        "[memory-levels] contextual_used_llm={} contextual_hit_count={} url_count={} path_count={} content=\n{}",
        contextual.used_llm,
        contextual.hits.len(),
        contextual.content.matches(image_url).count(),
        contextual.content.matches(file_path).count(),
        contextual.content
    );

    assert!(
        contextual.content.contains(image_url),
        "Tool 化上下文回忆应整理出可继续使用的媒体 URL"
    );
    assert!(
        contextual.content.contains(file_path),
        "Tool 化上下文回忆应整理出可继续使用的文件路径"
    );
    assert_eq!(
        contextual.content.matches(image_url).count(),
        1,
        "整理后的回忆结果应去重，避免重复输出同一 URL；实际内容:\n{}",
        contextual.content
    );
    assert!(
        !contextual.content.contains("当前上下文只包含用户请求"),
        "整理后的回忆结果不应复述当前上下文，避免浪费 prompt"
    );

    handle.shutdown().await;
}

fn compact_for_log(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}
