use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;
use tiangong_memory::{
    Episode, EpisodeOutcome, MemoryKind, MemoryRecallRequest, RecallAnchors, TurnArtifact,
    TurnArtifactKind, TurnResult, load_injection_sync, start, workspace_id_from_path,
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

async fn wait_for_recall_kind(
    handle: &tiangong_memory::MemoryHandle,
    query: &str,
    kind: MemoryKind,
) -> Vec<tiangong_memory::RecallHit> {
    for attempt in 1..=30 {
        let hits = handle
            .recall(
                RecallAnchors {
                    query: query.to_string(),
                    keywords: Vec::new(),
                    strategy: None,
                },
                8,
            )
            .await;
        eprintln!(
            "[wait-kind] attempt={attempt} query={query:?} kind={kind:?} hits={}",
            hits.len()
        );
        if hits.iter().any(|hit| hit.kind == kind) {
            return hits;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Vec::new()
}

async fn wait_for_unique_kind_hits(
    handle: &tiangong_memory::MemoryHandle,
    query: &str,
    kind: MemoryKind,
) -> Vec<tiangong_memory::RecallHit> {
    for attempt in 1..=30 {
        let hits = handle
            .recall(
                RecallAnchors {
                    query: query.to_string(),
                    keywords: Vec::new(),
                    strategy: None,
                },
                8,
            )
            .await;
        let kind_hits = hits
            .iter()
            .filter(|hit| hit.kind == kind)
            .collect::<Vec<_>>();
        let unique_ids = kind_hits
            .iter()
            .map(|hit| hit.node_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        eprintln!(
            "[wait-unique-kind] attempt={attempt} query={query:?} kind={kind:?} kind_hits={} unique_ids={}",
            kind_hits.len(),
            unique_ids.len()
        );
        if !kind_hits.is_empty() && kind_hits.len() == unique_ids.len() {
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
async fn artifact_only_turn_is_added_to_memory_and_recalled_by_context_tool() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let image_url = "https://example.invalid/artifacts/architecture-sunrise.png";
    let file_path = "/tmp/tiangong/architecture-sunrise.png";
    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");

    handle.run_micro_rumination(TurnResult {
        session_id: "session-artifact-add".to_string(),
        turn_id: "turn-artifact-only".to_string(),
        had_tool_calls: false,
        user_input: "展示刚生成的 architecture sunrise diagram".to_string(),
        summary: "assistant produced architecture sunrise diagram artifact".to_string(),
        tool_calls: Vec::new(),
        artifacts: vec![
            TurnArtifact {
                kind: TurnArtifactKind::Media,
                tool_name: Some("image_generation".to_string()),
                title: Some("architecture sunrise diagram".to_string()),
                url: Some(image_url.to_string()),
                path: None,
                summary: Some("final generated architecture diagram".to_string()),
            },
            TurnArtifact {
                kind: TurnArtifactKind::File,
                tool_name: Some("write_file".to_string()),
                title: Some("architecture sunrise local file".to_string()),
                url: None,
                path: Some(file_path.to_string()),
                summary: Some("local copy of generated diagram".to_string()),
            },
        ],
        workspace_id: Some(workspace_id.clone()),
    });
    eprintln!("[artifact-add] submitted artifact-only turn url={image_url} path={file_path}");

    let hits = wait_for_recall_hit(&handle, "architecture sunrise diagram").await;
    eprintln!("[artifact-add] hit_count={}", hits.len());
    for (idx, hit) in hits.iter().enumerate() {
        eprintln!(
            "[artifact-add] hit[{idx}] node_id={} score={:.3} title={} summary={}",
            hit.node_id, hit.score, hit.title, hit.summary
        );
    }
    assert!(
        hits.iter()
            .any(|hit| hit.summary.contains("architecture sunrise diagram")),
        "即使没有工具调用，只要 turn 含结构化产物，也应新增 Episode 记忆"
    );

    let expanded = handle
        .load_depth2(hits.iter().map(|hit| hit.node_id.clone()).collect())
        .await;
    let expanded_text = expanded
        .iter()
        .map(|item| item.full_content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!(
        "[artifact-add] expanded_contains_url={} expanded_contains_path={} expanded=\n{}",
        expanded_text.contains(image_url),
        expanded_text.contains(file_path),
        compact_for_log(&expanded_text, 900)
    );
    assert!(expanded_text.contains(image_url));
    assert!(expanded_text.contains(file_path));

    let contextual = handle
        .recall_context(MemoryRecallRequest {
            query: "刚才那个 architecture sunrise diagram 的图片和文件路径".to_string(),
            reason: Some("用户用历史指代继续使用刚生成的结构化产物".to_string()),
            expected: vec!["media".to_string(), "file".to_string()],
            context: vec!["当前上下文没有图片 URL，也没有本地文件路径".to_string()],
            limit: 5,
        })
        .await;
    eprintln!(
        "[artifact-add] contextual_hit_count={} used_llm={} content=\n{}",
        contextual.hits.len(),
        contextual.used_llm,
        contextual.content
    );
    assert!(
        contextual.content.contains(image_url),
        "按需回忆应返回结构化媒体 URL"
    );
    assert!(
        contextual.content.contains(file_path),
        "按需回忆应返回结构化文件路径"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn contextual_recall_returns_incremental_memory_without_repeating_prompt_context() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");
    let redundant_summary = "We chose coral teal palette for flamingo dashboard";
    let export_url = "https://example.invalid/artifacts/flamingo-dashboard.svg";
    let export_path = "/tmp/tiangong/flamingo-dashboard.svg";

    handle.write_episode(
        Episode::new(
            "session-incremental-recall".to_string(),
            "flamingo palette decision".to_string(),
            redundant_summary.to_string(),
            EpisodeOutcome::Success,
            vec![
                "flamingo".to_string(),
                "palette".to_string(),
                "dashboard".to_string(),
            ],
            vec!["recall_memory".to_string()],
            0.6,
        ),
        Some(workspace_id.clone()),
    );
    handle.write_episode(
        Episode::new(
            "session-incremental-recall".to_string(),
            "flamingo export artifact".to_string(),
            format!("Exported flamingo dashboard artifact {export_url} {export_path}"),
            EpisodeOutcome::Success,
            vec![
                "flamingo".to_string(),
                "artifact".to_string(),
                "dashboard".to_string(),
            ],
            vec!["generate_image".to_string(), "write_file".to_string()],
            0.8,
        ),
        Some(workspace_id.clone()),
    );
    eprintln!(
        "[incremental-recall] submitted redundant_summary={redundant_summary:?} export_url={export_url} export_path={export_path}"
    );

    let hits = wait_for_recall_hit(&handle, "flamingo dashboard artifact").await;
    eprintln!("[incremental-recall] precheck_hit_count={}", hits.len());
    assert!(
        hits.iter()
            .any(|hit| hit.summary.contains("flamingo dashboard artifact")),
        "测试前应能召回带 URL/路径的增量记忆"
    );

    let contextual = handle
        .recall_context(MemoryRecallRequest {
            query: "继续 flamingo dashboard，需要之前的导出产物".to_string(),
            reason: Some("当前上下文只有设计决策，缺少可继续使用的产物引用".to_string()),
            expected: vec!["file".to_string(), "media".to_string()],
            context: vec![
                "user: 继续 flamingo dashboard".to_string(),
                format!("assistant: {redundant_summary}"),
            ],
            limit: 5,
        })
        .await;
    eprintln!(
        "[incremental-recall] contextual_hit_count={} used_llm={} content=\n{}",
        contextual.hits.len(),
        contextual.used_llm,
        contextual.content
    );

    assert!(
        contextual.content.contains(export_url),
        "回忆结果应保留当前上下文之外的 URL 增量信息"
    );
    assert!(
        contextual.content.contains(export_path),
        "回忆结果应保留当前上下文之外的路径增量信息"
    );
    assert!(
        !contextual.content.contains(redundant_summary),
        "回忆结果不应重复当前上下文已包含的摘要，避免浪费 prompt"
    );
    assert_eq!(
        contextual.content.matches(export_url).count(),
        1,
        "同一 URL 在回忆结果中应只出现一次"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn contextual_recall_fixed_reference_cases_return_only_incremental_memory() {
    let cases = incremental_recall_cases();

    for case in &cases {
        let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
        let _env = EnvGuard::enter(home.path(), &workspace_path);
        let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");

        handle.write_episode(
            Episode::new(
                format!("session-incremental-{}", case.slug),
                format!("{} current context", case.slug),
                case.current_context.to_string(),
                EpisodeOutcome::Success,
                case.keywords(),
                vec!["recall_memory".to_string()],
                0.5,
            ),
            Some(workspace_id.clone()),
        );
        handle.write_episode(
            Episode::new(
                format!("session-incremental-{}", case.slug),
                format!("{} incremental memory", case.slug),
                case.incremental_memory.to_string(),
                EpisodeOutcome::Success,
                case.keywords(),
                case.tool_calls
                    .iter()
                    .map(|item| item.to_string())
                    .collect(),
                0.8,
            ),
            Some(workspace_id.clone()),
        );

        let hits = wait_for_recall_hit(&handle, case.query).await;
        assert!(
            hits.iter()
                .any(|hit| hit.summary.contains(case.expected_refs[0])),
            "固定样例 {} 应先能召回增量记忆",
            case.slug
        );

        let contextual = handle
            .recall_context(MemoryRecallRequest {
                query: case.query.to_string(),
                reason: Some(case.reason.to_string()),
                expected: case.expected.iter().map(|item| item.to_string()).collect(),
                context: vec![
                    format!("user: {}", case.user_context),
                    format!("assistant: {}", case.current_context),
                ],
                limit: 5,
            })
            .await;
        eprintln!(
            "[incremental-case:{}] used_llm={} hit_count={} content=\n{}",
            case.slug,
            contextual.used_llm,
            contextual.hits.len(),
            contextual.content
        );

        assert!(
            !contextual.content.contains(case.current_context),
            "固定样例 {} 不应重复当前上下文已包含的摘要",
            case.slug
        );
        for expected_ref in case.expected_refs {
            assert!(
                contextual.content.contains(expected_ref),
                "固定样例 {} 应返回增量引用 {}",
                case.slug,
                expected_ref
            );
            assert_eq!(
                contextual.content.matches(expected_ref).count(),
                1,
                "固定样例 {} 的增量引用 {} 应只出现一次",
                case.slug,
                expected_ref
            );
        }

        handle.shutdown().await;
    }
}

struct IncrementalRecallCase {
    slug: &'static str,
    query: &'static str,
    user_context: &'static str,
    current_context: &'static str,
    incremental_memory: &'static str,
    expected_refs: &'static [&'static str],
    expected: &'static [&'static str],
    tool_calls: &'static [&'static str],
    reason: &'static str,
}

impl IncrementalRecallCase {
    fn keywords(&self) -> Vec<String> {
        self.query
            .split_whitespace()
            .chain([self.slug])
            .map(str::to_string)
            .collect()
    }
}

fn incremental_recall_cases() -> Vec<IncrementalRecallCase> {
    vec![
        IncrementalRecallCase {
            slug: "flamingo-export",
            query: "继续 flamingo-export dashboard 的导出产物",
            user_context: "继续 flamingo-export dashboard",
            current_context: "flamingo-export 已确定 coral teal palette",
            incremental_memory: "flamingo-export 导出产物位于 https://example.invalid/artifacts/flamingo-export.svg path=/tmp/tiangong/flamingo-export.svg",
            expected_refs: &[
                "https://example.invalid/artifacts/flamingo-export.svg",
                "/tmp/tiangong/flamingo-export.svg",
            ],
            expected: &["media", "file"],
            tool_calls: &["generate_image", "write_file"],
            reason: "用户用历史指代继续使用刚生成的 dashboard 产物",
        },
        IncrementalRecallCase {
            slug: "profile-migration",
            query: "继续 profile-migration 上次那个迁移文件",
            user_context: "继续 profile-migration 上次那个迁移",
            current_context: "profile-migration 决定拆成 schema 和 backfill 两步",
            incremental_memory: "profile-migration 迁移文件保存为 /workspace/migrations/20260506_profile_migration.sql，回滚说明在 /workspace/docs/profile_migration_rollback.md",
            expected_refs: &[
                "/workspace/migrations/20260506_profile_migration.sql",
                "/workspace/docs/profile_migration_rollback.md",
            ],
            expected: &["file"],
            tool_calls: &["write_file"],
            reason: "用户提到上次那个迁移，需要补回当前上下文缺失的文件路径",
        },
        IncrementalRecallCase {
            slug: "raven-poster",
            query: "把 raven-poster 刚才那张图再导出一次",
            user_context: "把 raven-poster 刚才那张图再导出一次",
            current_context: "raven-poster 使用 night garden prompt",
            incremental_memory: "raven-poster 图片地址是 https://example.invalid/media/raven-poster.png，本地副本 path=/tmp/tiangong/raven-poster.png",
            expected_refs: &[
                "https://example.invalid/media/raven-poster.png",
                "/tmp/tiangong/raven-poster.png",
            ],
            expected: &["media", "file"],
            tool_calls: &["generate_image"],
            reason: "用户用刚才那张图指代历史图片产物",
        },
        IncrementalRecallCase {
            slug: "mcp-config",
            query: "沿用 mcp-config 之前那个服务器配置",
            user_context: "沿用 mcp-config 之前那个服务器配置",
            current_context: "mcp-config 选择 stdio transport",
            incremental_memory: "mcp-config 配置文件在 /workspace/.tiangong/mcp/linear-server.json，endpoint 说明见 /workspace/docs/mcp-linear.md",
            expected_refs: &[
                "/workspace/.tiangong/mcp/linear-server.json",
                "/workspace/docs/mcp-linear.md",
            ],
            expected: &["file"],
            tool_calls: &["write_file"],
            reason: "用户提到之前那个服务器配置，需要返回可继续使用的配置文件",
        },
        IncrementalRecallCase {
            slug: "latency-trace",
            query: "继续 latency-trace 上次的性能排查",
            user_context: "继续 latency-trace 上次的性能排查",
            current_context: "latency-trace 判定瓶颈在 recall rerank 阶段",
            incremental_memory: "latency-trace profile 数据位于 /tmp/tiangong/latency-trace.json，火焰图为 https://example.invalid/perf/latency-trace.html",
            expected_refs: &[
                "/tmp/tiangong/latency-trace.json",
                "https://example.invalid/perf/latency-trace.html",
            ],
            expected: &["file"],
            tool_calls: &["run_command", "write_file"],
            reason: "用户继续性能排查，需要补回 profile 和火焰图引用",
        },
        IncrementalRecallCase {
            slug: "skill-template",
            query: "用 skill-template 上次那个技能模板继续生成",
            user_context: "用 skill-template 上次那个技能模板继续生成",
            current_context: "skill-template 采用 plan first 的写法",
            incremental_memory: "skill-template 模板文件保存为 /workspace/.tiangong/skills/memory-review/SKILL.md，示例输入在 /workspace/docs/skill-template-example.md",
            expected_refs: &[
                "/workspace/.tiangong/skills/memory-review/SKILL.md",
                "/workspace/docs/skill-template-example.md",
            ],
            expected: &["file"],
            tool_calls: &["write_file"],
            reason: "用户用上次那个技能模板指代历史文件产物",
        },
    ]
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn meso_rumination_extracts_entity_and_decision_memories() {
    let (home, _workspace, workspace_path, workspace_id) = setup_workspace();
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start(Some(workspace_id.clone())).expect("启动 memory 失败");
    handle.write_episode(
        Episode::new(
            "session-meso".to_string(),
            "qdrant vector backend evaluation".to_string(),
            "Compared external qdrant server with embedded flat vector index for memory recall."
                .to_string(),
            EpisodeOutcome::Success,
            vec![
                "qdrant".to_string(),
                "vector".to_string(),
                "memory".to_string(),
            ],
            vec!["cargo_test".to_string()],
            0.7,
        ),
        Some(workspace_id.clone()),
    );
    handle.write_episode(
        Episode::new(
            "session-meso".to_string(),
            "choose embedded vector index for memory".to_string(),
            "We decided to choose embedded vector index instead of external qdrant server to reduce user startup complexity."
                .to_string(),
            EpisodeOutcome::Success,
            vec![
                "qdrant".to_string(),
                "vector".to_string(),
                "memory".to_string(),
                "embedded".to_string(),
            ],
            vec!["architecture_decision".to_string()],
            0.85,
        ),
        Some(workspace_id.clone()),
    );
    eprintln!("[meso] submitted source episodes workspace_id={workspace_id}");

    handle.run_meso_rumination("session-meso".to_string(), workspace_id.clone());

    let entity_hits =
        wait_for_recall_kind(&handle, "qdrant memory entity", MemoryKind::Entity).await;
    for (idx, hit) in entity_hits.iter().enumerate() {
        eprintln!(
            "[meso] entity_hit[{idx}] kind={:?} title={} summary={}",
            hit.kind, hit.title, hit.summary
        );
    }
    assert!(
        entity_hits.iter().any(|hit| hit.kind == MemoryKind::Entity),
        "Meso 反刍应从近期 Episode 提炼 Entity 并写入可召回索引"
    );

    let decision_hits = wait_for_recall_kind(
        &handle,
        "choose embedded vector index decision",
        MemoryKind::Decision,
    )
    .await;
    for (idx, hit) in decision_hits.iter().enumerate() {
        eprintln!(
            "[meso] decision_hit[{idx}] kind={:?} title={} summary={}",
            hit.kind, hit.title, hit.summary
        );
    }
    let decision = decision_hits
        .iter()
        .find(|hit| hit.kind == MemoryKind::Decision)
        .expect("应召回 Decision 节点");
    let expanded = handle.load_depth2(vec![decision.node_id.clone()]).await;
    let expanded_text = expanded
        .iter()
        .map(|item| item.full_content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("[meso] decision_depth2={expanded_text}");
    assert!(
        expanded_text.contains("embedded vector index"),
        "Decision 完整内容应保留 chosen/context 等决策线索"
    );

    let injection = handle
        .load_injection("session-meso", Some(&workspace_id))
        .await
        .join("\n");
    eprintln!("[meso] workspace_injection=\n{injection}");
    assert!(injection.contains("实体记忆"));
    assert!(injection.contains("决策记忆"));

    handle.run_meso_rumination("session-meso".to_string(), workspace_id.clone());
    eprintln!("[meso] rerun submitted for idempotency verification");

    let rerun_entity_hits =
        wait_for_unique_kind_hits(&handle, "qdrant memory entity", MemoryKind::Entity).await;
    let entity_ids = rerun_entity_hits
        .iter()
        .filter(|hit| hit.kind == MemoryKind::Entity)
        .map(|hit| hit.node_id.as_str())
        .collect::<Vec<_>>();
    let unique_entity_ids = entity_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    eprintln!(
        "[meso] rerun_entity_hits={} unique_entity_ids={} ids={entity_ids:?}",
        entity_ids.len(),
        unique_entity_ids.len()
    );
    assert_eq!(
        entity_ids.len(),
        unique_entity_ids.len(),
        "重复运行 Meso 后 Entity 搜索结果不应出现同一 node_id 的重复命中"
    );

    let rerun_decision_hits = wait_for_unique_kind_hits(
        &handle,
        "choose embedded vector index decision",
        MemoryKind::Decision,
    )
    .await;
    let decision_ids = rerun_decision_hits
        .iter()
        .filter(|hit| hit.kind == MemoryKind::Decision)
        .map(|hit| hit.node_id.as_str())
        .collect::<Vec<_>>();
    let unique_decision_ids = decision_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    eprintln!(
        "[meso] rerun_decision_hits={} unique_decision_ids={} ids={decision_ids:?}",
        decision_ids.len(),
        unique_decision_ids.len()
    );
    assert_eq!(
        decision_ids.len(),
        unique_decision_ids.len(),
        "重复运行 Meso 后 Decision 搜索结果不应出现同一 node_id 的重复命中"
    );
    assert_eq!(
        unique_decision_ids.len(),
        1,
        "同一 Episode 的决策记忆重复反刍后仍应保持单个 Decision 节点"
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
