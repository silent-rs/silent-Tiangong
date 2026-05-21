use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use std::{
    io::{Read, Write},
    net::TcpListener,
};

use serial_test::serial;
use tempfile::TempDir;
use tiangong_llm::{EmbeddingEndpointConfig, ProviderProtocol};
use tiangong_memory::{
    Episode, EpisodeOutcome, MemoryOptions, MemoryStatus, MemoryVectorMode, RecallAnchors,
    start_with_options, workspace_id_from_path,
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

fn embedding_config_from_tiangong_config() -> Option<EmbeddingEndpointConfig> {
    let config = tiangong_config::load_tiangong_config();
    let endpoint = config.to_core_config().llm.embedding?;
    let dimension = endpoint
        .options
        .get("dimension")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())?;

    Some(EmbeddingEndpointConfig {
        base_url: endpoint.base_url,
        api_key: endpoint.api_key,
        model: endpoint.model,
        protocol: endpoint.protocol,
        timeout: Duration::from_millis(endpoint.timeout_ms),
        dimension,
    })
}

struct DeterministicEmbeddingServer {
    base_url: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl DeterministicEmbeddingServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 deterministic embedding 失败");
        let addr = listener.local_addr().expect("读取 embedding mock 地址失败");
        listener
            .set_nonblocking(true)
            .expect("设置 embedding mock 非阻塞失败");
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let body = read_http_body(&mut stream)
                            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                            .unwrap_or_else(|| serde_json::json!({}));
                        let inputs = body
                            .get("input")
                            .and_then(serde_json::Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let data = inputs
                            .iter()
                            .enumerate()
                            .map(|(index, item)| {
                                let text = item.as_str().unwrap_or_default();
                                serde_json::json!({
                                    "object": "embedding",
                                    "index": index,
                                    "embedding": deterministic_embedding(text)
                                })
                            })
                            .collect::<Vec<_>>();
                        let payload = serde_json::json!({
                            "object": "list",
                            "data": data,
                            "model": "deterministic-memory-embedding"
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            payload.len(),
                            payload
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }

    fn config(&self) -> EmbeddingEndpointConfig {
        EmbeddingEndpointConfig {
            base_url: self.base_url.clone(),
            api_key: "deterministic-test-key".to_string(),
            model: "deterministic-memory-embedding".to_string(),
            protocol: ProviderProtocol::OpenAiCompatible,
            timeout: Duration::from_secs(5),
            dimension: 4,
        }
    }
}

impl Drop for DeterministicEmbeddingServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn read_http_body(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(buffer[header_end..header_end + content_length].to_vec()).ok()
}

fn deterministic_embedding(text: &str) -> Vec<f32> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("dimension")
        || lower.contains("inference server")
        || lower.contains("settings form")
        || lower.contains("automatic form")
    {
        vec![1.0, 0.0, 0.0, 0.0]
    } else if lower.contains("checksum") || lower.contains("apple") || lower.contains("banana") {
        vec![0.0, 1.0, 0.0, 0.0]
    } else if lower.contains("redis")
        || lower.contains("sentinel")
        || lower.contains("cluster guardian")
        || lower.contains("continuity")
    {
        vec![0.0, 0.0, 1.0, 0.0]
    } else {
        vec![0.0, 0.0, 0.0, 1.0]
    }
}

async fn wait_for_expected_hit(
    handle: &tiangong_memory::MemoryHandle,
    query: &str,
    expected_node_id: &str,
) -> Vec<tiangong_memory::RecallHit> {
    for attempt in 1..=60 {
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
        println!(
            "[recall attempt {attempt}] query={query:?}, hits={}",
            hits.len()
        );
        for (idx, hit) in hits.iter().enumerate() {
            println!(
                "  #{idx}: id={} score={:.4} title={} summary={}",
                hit.node_id, hit.score, hit.title, hit.summary
            );
        }
        if hits.iter().any(|hit| hit.node_id == expected_node_id) {
            return hits;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Vec::new()
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn embedded_hybrid_retrieval_uses_deterministic_embedding_without_external_service() {
    let embedding_server = DeterministicEmbeddingServer::start();
    let home = TempDir::new().expect("创建 fake home 失败");
    let workspace = TempDir::new().expect("创建 workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    let handle = start_with_options(
        MemoryOptions::new()
            .with_embedding(embedding_server.config())
            .with_vector_mode(MemoryVectorMode::EmbeddedLanceDb),
    )
    .expect("启动 memory 失败");

    let semantic_episode = Episode::new(
        "hybrid-deterministic-session".to_string(),
        "settings vector probe workflow".to_string(),
        "Count the returned vector length from the inference server response and fill the settings form automatically."
            .to_string(),
        EpisodeOutcome::Success,
        vec!["settings".to_string(), "probe".to_string()],
        vec!["probe_embedding_dimension".to_string()],
        0.9,
    );
    let expected_id = semantic_episode.id.clone();
    handle.write_episode(semantic_episode, Some(workspace_id.clone()));
    handle.write_episode(
        Episode::new(
            "hybrid-deterministic-session".to_string(),
            "lexical sentinel unrelated checksum".to_string(),
            "apple banana checksum marker for pure keyword recall control".to_string(),
            EpisodeOutcome::Success,
            vec!["checksum".to_string()],
            vec!["noop".to_string()],
            0.2,
        ),
        Some(workspace_id),
    );

    let hits =
        wait_for_expected_hit(&handle, "automatic form dimension discovery", &expected_id).await;
    handle.shutdown().await;

    assert_eq!(
        hits.first().map(|hit| hit.node_id.as_str()),
        Some(expected_id.as_str()),
        "deterministic embedding 应让 hybrid 召回把语义相关 Episode 排到第一"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn recall_benchmark_compares_bm25_only_and_hybrid_hit_rate() {
    let embedding_server = DeterministicEmbeddingServer::start();
    let bm25_score = benchmark_recall(false, None).await;
    let hybrid_score = benchmark_recall(true, Some(embedding_server.config())).await;

    println!(
        "[recall-benchmark] bm25_hits={} hybrid_hits={}",
        bm25_score, hybrid_score
    );
    assert!(
        hybrid_score > bm25_score,
        "hybrid 应在语义指代样例上优于 BM25-only"
    );
    assert_eq!(hybrid_score, 3, "hybrid 应命中全部固定语义样例");
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn archived_node_is_removed_from_embedded_vector_index_and_can_be_restored() {
    let embedding_server = DeterministicEmbeddingServer::start();
    let home = TempDir::new().expect("创建 fake home 失败");
    let workspace = TempDir::new().expect("创建 workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    let _env = EnvGuard::enter(home.path(), &workspace_path);
    let handle = start_with_options(
        MemoryOptions::new()
            .with_embedding(embedding_server.config())
            .with_vector_mode(MemoryVectorMode::EmbeddedLanceDb),
    )
    .expect("启动 memory 失败");

    let episode = Episode::new(
        "hybrid-archive-session".to_string(),
        "settings vector archive target".to_string(),
        "Count the returned vector length from the inference server response and fill the settings form automatically."
            .to_string(),
        EpisodeOutcome::Success,
        vec!["settings".to_string(), "probe".to_string()],
        vec!["probe_embedding_dimension".to_string()],
        0.9,
    );
    let node_id = episode.id.clone();
    handle.write_episode(episode, Some(workspace_id));
    let initial_hits =
        wait_for_expected_hit(&handle, "automatic form dimension discovery", &node_id).await;
    assert!(
        initial_hits.iter().any(|hit| hit.node_id == node_id),
        "归档前应能通过 embedded vector 召回目标节点"
    );

    handle
        .set_node_status(node_id.clone(), MemoryStatus::Archived)
        .await
        .expect("归档节点应成功");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let archived_hits = handle
        .recall(
            RecallAnchors {
                query: "automatic form dimension discovery".to_string(),
                keywords: Vec::new(),
                strategy: None,
            },
            5,
        )
        .await;
    assert!(
        archived_hits.iter().all(|hit| hit.node_id != node_id),
        "归档后目标节点不应继续从 SQLite/Tantivy/embedded vector 召回"
    );

    handle
        .set_node_status(node_id.clone(), MemoryStatus::Active)
        .await
        .expect("恢复节点应成功");
    let restored_hits =
        wait_for_expected_hit(&handle, "automatic form dimension discovery", &node_id).await;
    handle.shutdown().await;
    assert!(
        restored_hits.iter().any(|hit| hit.node_id == node_id),
        "恢复 active 后应重建当前向量索引并重新召回目标节点"
    );
}

async fn benchmark_recall(hybrid: bool, embedding: Option<EmbeddingEndpointConfig>) -> usize {
    let home = TempDir::new().expect("创建 benchmark fake home 失败");
    let workspace = TempDir::new().expect("创建 benchmark workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    let _env = EnvGuard::enter(home.path(), &workspace_path);
    let mut options = MemoryOptions::new();
    if hybrid {
        options = options
            .with_embedding(embedding.expect("hybrid benchmark 需要 embedding"))
            .with_vector_mode(MemoryVectorMode::EmbeddedLanceDb);
    }
    let handle = start_with_options(options).expect("启动 benchmark memory 失败");
    let cases = [
        (
            "settings vector probe workflow",
            "Count the returned vector length from the inference server response and fill the settings form automatically.",
            "dimension discovery",
        ),
        (
            "redis failover deployment",
            "Redis Sentinel manages primary replica failover and stores the sentinel deployment file.",
            "cluster guardian continuity",
        ),
        (
            "checksum control marker",
            "apple banana checksum marker for keyword-only baseline control",
            "apple banana checksum",
        ),
    ];
    let mut expected = Vec::new();
    for (title, summary, _) in cases {
        let episode = Episode::new(
            "hybrid-benchmark-session".to_string(),
            title.to_string(),
            summary.to_string(),
            EpisodeOutcome::Success,
            vec!["benchmark".to_string()],
            vec!["benchmark".to_string()],
            0.8,
        );
        expected.push(episode.id.clone());
        handle.write_episode(episode, Some(workspace_id.clone()));
    }

    let mut best_hit_count = 0;
    for _ in 1..=60 {
        let mut hit_count = 0;
        for (idx, (_, _, query)) in cases.iter().enumerate() {
            let hits = handle
                .recall(
                    RecallAnchors {
                        query: query.to_string(),
                        keywords: Vec::new(),
                        strategy: None,
                    },
                    3,
                )
                .await;
            if hits.first().is_some_and(|hit| hit.node_id == expected[idx]) {
                hit_count += 1;
            }
        }
        best_hit_count = best_hit_count.max(hit_count);
        if best_hit_count == cases.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    handle.shutdown().await;
    best_hit_count
}

#[tokio::test(flavor = "current_thread")]
#[serial]
#[ignore = "需要真实 ~/.tiangong/models.json embedding 配置；使用 --ignored --nocapture 运行"]
async fn embedded_hybrid_retrieval_loads_configured_embedding_and_recalls_semantic_episode() {
    let Some(embedding) = embedding_config_from_tiangong_config() else {
        println!("[skip] 未在配置文件中找到 embedding 路由或 options.dimension");
        return;
    };
    if embedding.protocol != ProviderProtocol::OpenAiCompatible {
        println!(
            "[skip] embedding 协议不是 OpenAI 兼容协议: {}",
            embedding.protocol.as_str()
        );
        return;
    }

    let home = TempDir::new().expect("创建 fake home 失败");
    let workspace = TempDir::new().expect("创建 workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    let _env = EnvGuard::enter(home.path(), &workspace_path);

    println!(
        "[config] embedding_model={} dimension={} backend=embedded_flat",
        embedding.model, embedding.dimension
    );

    let handle = start_with_options(
        MemoryOptions::new()
            .with_embedding(embedding)
            .with_vector_mode(MemoryVectorMode::EmbeddedLanceDb),
    )
    .expect("启动 memory 失败");

    let semantic_episode = Episode::new(
        "hybrid-session".to_string(),
        "vector length probe for settings dialog".to_string(),
        "Count the returned vector length from the inference server response and fill the settings form automatically."
            .to_string(),
        EpisodeOutcome::Success,
        vec!["settings".to_string(), "probe".to_string()],
        vec!["probe_embedding_dimension".to_string()],
        0.9,
    );
    let expected_id = semantic_episode.id.clone();
    println!("[write] semantic_episode_id={expected_id}");
    handle.write_episode(semantic_episode, Some(workspace_id.clone()));

    handle.write_episode(
        Episode::new(
            "hybrid-session".to_string(),
            "lexical sentinel unrelated checksum".to_string(),
            "apple banana checksum marker for pure keyword recall control".to_string(),
            EpisodeOutcome::Success,
            vec!["checksum".to_string()],
            vec!["noop".to_string()],
            0.2,
        ),
        Some(workspace_id),
    );

    let hits = wait_for_expected_hit(&handle, "embedding dimension", &expected_id).await;
    handle.shutdown().await;

    assert!(
        hits.iter().any(|hit| hit.node_id == expected_id),
        "混合检索应通过配置的 embedding + 内置向量索引召回语义相关 Episode"
    );
}
