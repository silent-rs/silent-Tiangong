use std::path::{Path, PathBuf};
use std::time::Duration;

use qdrant_client::Qdrant;
use serial_test::serial;
use tempfile::TempDir;
use tiangong_llm::ProviderProtocol;
use tiangong_memory::{
    Episode, EpisodeOutcome, MemoryEmbeddingConfig, MemoryOptions, RecallAnchors,
    start_with_options, workspace_id_from_path,
};

struct EnvGuard {
    prev_home: Option<std::ffi::OsString>,
    prev_userprofile: Option<std::ffi::OsString>,
    prev_collection: Option<std::ffi::OsString>,
    prev_cwd: PathBuf,
}

impl EnvGuard {
    fn enter(home: &Path, cwd: &Path, collection: &str) -> Self {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        let prev_collection = std::env::var_os("TIANGONG_MEMORY_QDRANT_COLLECTION");
        let prev_cwd = std::env::current_dir().expect("读取当前工作目录失败");

        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
            std::env::set_var("TIANGONG_MEMORY_QDRANT_COLLECTION", collection);
        }
        std::env::set_current_dir(cwd).expect("切换当前工作目录失败");

        Self {
            prev_home,
            prev_userprofile,
            prev_collection,
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
            match &self.prev_collection {
                Some(value) => std::env::set_var("TIANGONG_MEMORY_QDRANT_COLLECTION", value),
                None => std::env::remove_var("TIANGONG_MEMORY_QDRANT_COLLECTION"),
            }
        }
    }
}

fn embedding_config_from_tiangong_config() -> Option<MemoryEmbeddingConfig> {
    let config = tiangong_config::load_tiangong_config();
    let endpoint = config.to_core_config().llm.embedding?;
    let dimension = endpoint
        .options
        .get("dimension")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())?;

    Some(MemoryEmbeddingConfig {
        base_url: endpoint.base_url,
        api_key: endpoint.api_key,
        model: endpoint.model,
        protocol: endpoint.protocol,
        timeout_ms: endpoint.timeout_ms,
        dimension,
    })
}

async fn prepare_qdrant_collection(collection: &str) -> Option<Qdrant> {
    let url = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_string());
    let client = match Qdrant::from_url(&url).build() {
        Ok(client) => client,
        Err(err) => {
            println!("[skip] 无法创建 Qdrant client: url={url}, error={err}");
            return None;
        }
    };
    if let Err(err) = client.list_collections().await {
        println!("[skip] Qdrant 不可用: url={url}, error={err}");
        return None;
    }
    let _ = client.delete_collection(collection).await;
    Some(client)
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
#[ignore = "需要真实 ~/.tiangong/models.json embedding 配置和 Qdrant 服务；使用 --ignored --nocapture 运行"]
async fn hybrid_retrieval_loads_configured_embedding_and_recalls_semantic_episode() {
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

    let collection = format!("tiangong_memory_test_{}", scru128::new());
    let Some(qdrant) = prepare_qdrant_collection(&collection).await else {
        return;
    };

    let home = TempDir::new().expect("创建 fake home 失败");
    let workspace = TempDir::new().expect("创建 workspace 失败");
    let workspace_path = workspace.path().to_path_buf();
    let workspace_id = workspace_id_from_path(&workspace_path);
    let _env = EnvGuard::enter(home.path(), &workspace_path, &collection);

    println!(
        "[config] embedding_model={} dimension={} collection={}",
        embedding.model, embedding.dimension, collection
    );

    let handle = start_with_options(
        MemoryOptions::new(Some(workspace_id.clone())).with_embedding(embedding),
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
    let _ = qdrant.delete_collection(&collection).await;

    assert!(
        hits.iter().any(|hit| hit.node_id == expected_id),
        "混合检索应通过配置的 embedding + Qdrant 召回语义相关 Episode"
    );
}
