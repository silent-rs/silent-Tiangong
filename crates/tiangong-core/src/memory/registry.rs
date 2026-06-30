//! Memory Handle 全局单例：初始化、热更新、关闭

use std::sync::{Mutex, OnceLock};

use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::session::now_text;

static MEMORY_HANDLE: OnceLock<Mutex<Option<MemoryEntry>>> = OnceLock::new();
static MEMORY_INIT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct MemoryEntry {
    managed: tiangong_memory::ManagedMemory,
    handle: tiangong_memory::MemoryHandle,
    config_summary: MemoryConfigSummary,
    config_generation: u64,
    created_at: String,
    last_used_at: String,
    restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryConfigSummary {
    model: Option<MemoryModelSummary>,
    embedding: Option<MemoryEmbeddingSummary>,
    rerank: Option<MemoryRerankSummary>,
    vector_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryModelSummary {
    base_url: String,
    model: String,
    protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryEmbeddingSummary {
    base_url: String,
    model: String,
    protocol: String,
    dimension: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryRerankSummary {
    base_url: String,
    model: String,
    protocol: String,
}

pub(crate) struct WorkerMemoryContext {
    pub handle: Option<tiangong_memory::MemoryHandle>,
    pub process_type: tiangong_memory::ProcessType,
    pub initial_config_snapshot: Option<std::sync::Arc<crate::core_config::CoreConfig>>,
    pub initial_config_generation: u64,
}

/// 获取或初始化全局 Memory Handle（异步）。
pub(crate) async fn get_or_init_memory_async(
    config: &CoreConfig,
    config_generation: u64,
    process_type: tiangong_memory::ProcessType,
) -> Option<tiangong_memory::MemoryHandle> {
    // 运行时禁用检查：`tiangong memory disable` 会创建标记文件。
    // 标记存在时跳过 Memory 启动，全局生效（CLI/Server/Desktop）。
    if tiangong_memory::is_memory_disabled() {
        tracing::info!("Memory 已被禁用标记关闭（memory/.disabled），跳过启动");
        return None;
    }

    let options = config.to_memory_options();
    let config_summary = memory_config_summary_from_options(&options);
    let slot = MEMORY_HANDLE.get_or_init(|| Mutex::new(None));

    let existing = {
        let mut guard = match slot.lock() {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!("Memory Handle registry 已污染，尝试恢复: {}", err);
                err.into_inner()
            }
        };
        if let Some(entry) = guard.as_mut() {
            entry.last_used_at = now_text();
            let summary_changed = memory_config_changed(&entry.config_summary, &config_summary);
            let generation_changed = entry.config_generation != config_generation;
            if generation_changed {
                entry.config_generation = config_generation;
            }
            let handle = entry.handle.clone();
            if summary_changed
                && memory_config_can_update_in_place(&entry.config_summary, &config_summary)
            {
                Some((handle, true))
            } else {
                if summary_changed {
                    entry.restart_required = true;
                    tracing::warn!(
                        created_at = %entry.created_at,
                        last_used_at = %entry.last_used_at,
                        "Memory 配置变化需要重启 actor，当前继续复用旧 handle 并标记待重启"
                    );
                }
                Some((handle, false))
            }
        } else {
            None
        }
    };

    if let Some((handle, should_reconfigure)) = existing {
        if should_reconfigure {
            match handle.reconfigure(options).await {
                Ok(()) => {
                    let mut guard = match slot.lock() {
                        Ok(guard) => guard,
                        Err(err) => {
                            tracing::warn!("Memory Handle registry 已污染，尝试恢复: {}", err);
                            err.into_inner()
                        }
                    };
                    if let Some(entry) = guard.as_mut()
                        && entry.handle.is_same_handle(&handle)
                    {
                        entry.config_summary = config_summary;
                        entry.restart_required = false;
                        tracing::info!(
                            created_at = %entry.created_at,
                            last_used_at = %entry.last_used_at,
                            "Memory 配置已原地热更新"
                        );
                    }
                }
                Err(err) => {
                    let mut guard = match slot.lock() {
                        Ok(guard) => guard,
                        Err(err) => {
                            tracing::warn!("Memory Handle registry 已污染，尝试恢复: {}", err);
                            err.into_inner()
                        }
                    };
                    if let Some(entry) = guard.as_mut()
                        && entry.handle.is_same_handle(&handle)
                    {
                        entry.restart_required = true;
                        tracing::warn!(
                            created_at = %entry.created_at,
                            last_used_at = %entry.last_used_at,
                            "Memory 配置热更新失败，继续复用旧 handle 并标记待重启: {}", err
                        );
                    }
                }
            }
        }
        return Some(handle);
    }

    let init_lock = MEMORY_INIT_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _init_guard = init_lock.lock().await;

    let existing_after_wait = {
        let mut guard = match slot.lock() {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!("Memory Handle registry 已污染，尝试恢复: {}", err);
                err.into_inner()
            }
        };
        guard.as_mut().map(|entry| {
            entry.last_used_at = now_text();
            entry.handle.clone()
        })
    };
    if let Some(handle) = existing_after_wait {
        return Some(handle);
    }

    match tiangong_memory::start_or_connect_with_options(options, process_type).await {
        Ok(managed) => {
            let handle = managed.handle();
            let is_leader = managed.is_leader();
            tracing::info!(is_leader, "Memory 已启动或连接");
            let mut guard = match slot.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    tracing::warn!("Memory Handle registry 已污染，尝试恢复: {}", err);
                    err.into_inner()
                }
            };
            if let Some(entry) = guard.as_mut() {
                entry.last_used_at = now_text();
                return Some(entry.handle.clone());
            }
            let now = now_text();
            *guard = Some(MemoryEntry {
                managed,
                handle: handle.clone(),
                config_summary,
                config_generation,
                created_at: now.clone(),
                last_used_at: now,
                restart_required: false,
            });
            Some(handle)
        }
        Err(err) => {
            tracing::debug!("Memory 启动或连接失败（非致命）: {}", err);
            None
        }
    }
}

/// 异步获取或初始化全局 Memory Handle，供 Tauri async command 复用。
pub async fn get_or_init_memory_handle_async(
    config_provider: &CoreConfigProvider,
) -> Option<tiangong_memory::MemoryHandle> {
    let config = config_provider.snapshot();
    get_or_init_memory_async(
        &config,
        config_provider.generation(),
        tiangong_memory::ProcessType::Gui,
    )
    .await
}

/// 通过 Core 读取 Memory 独立配置，供 GUI 配置页展示。
pub fn load_memory_config() -> tiangong_memory::MemoryConfig {
    tiangong_memory::MemoryConfig::load_or_default()
}

/// 通过 Core 保存 Memory 独立配置，避免 GUI 入口直接操作 Memory 配置文件。
pub fn save_memory_config(config: tiangong_memory::MemoryConfig) -> anyhow::Result<()> {
    config.save()
}

/// 统一关闭当前进程内 MemoryHandle。
pub fn shutdown_memory_registry_blocking() {
    let Some(slot) = MEMORY_HANDLE.get() else {
        return;
    };
    let entry = match slot.lock() {
        Ok(mut guard) => guard.take(),
        Err(err) => {
            tracing::warn!("Memory Handle registry 已污染，尝试恢复后统一关闭: {}", err);
            err.into_inner().take()
        }
    };
    let Some(entry) = entry else {
        return;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        match std::thread::Builder::new()
            .name("memory-shutdown".to_string())
            .spawn(move || shutdown_memory_entry_blocking(entry))
        {
            Ok(join) => {
                if join.join().is_err() {
                    tracing::warn!("Memory shutdown 线程 panic");
                }
            }
            Err(err) => tracing::warn!("Memory shutdown 线程创建失败: {}", err),
        }
        return;
    }
    shutdown_memory_entry_blocking(entry);
}

fn shutdown_memory_entry_blocking(entry: MemoryEntry) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!("Memory shutdown runtime 构建失败: {}", err);
            return;
        }
    };
    runtime.block_on(async move {
        if entry.managed.is_leader() {
            entry.handle.shutdown().await;
        }
    });
}

fn memory_config_changed(running: &MemoryConfigSummary, latest: &MemoryConfigSummary) -> bool {
    running != latest
}

#[cfg(test)]
fn memory_config_summary(_config: &CoreConfig) -> MemoryConfigSummary {
    let options = tiangong_memory::MemoryConfig::load_or_default().to_options();
    memory_config_summary_from_options(&options)
}

fn memory_config_summary_from_options(
    options: &tiangong_memory::MemoryOptions,
) -> MemoryConfigSummary {
    MemoryConfigSummary {
        model: options.model.as_ref().map(|model| MemoryModelSummary {
            base_url: model.base_url.clone(),
            model: model.model.clone(),
            protocol: format!("{:?}", model.protocol),
        }),
        embedding: options
            .embedding
            .as_ref()
            .map(|embedding| MemoryEmbeddingSummary {
                base_url: embedding.base_url.clone(),
                model: embedding.model.clone(),
                protocol: format!("{:?}", embedding.protocol),
                dimension: embedding.dimension,
            }),
        rerank: options.rerank.as_ref().map(|rerank| MemoryRerankSummary {
            base_url: rerank.base_url.clone(),
            model: rerank.model.clone(),
            protocol: format!("{:?}", rerank.protocol),
        }),
        vector_mode: format!("{:?}", options.vector_mode),
    }
}

fn memory_config_can_update_in_place(
    _running: &MemoryConfigSummary,
    _latest: &MemoryConfigSummary,
) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_config::{CoreConfig, CoreConfigProvider};

    static MEMORY_REGISTRY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn memory_registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
        MEMORY_REGISTRY_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("memory registry 测试锁已污染")
    }

    #[test]
    fn memory_config_summary_tracks_memory_relevant_fields() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        write_memory_test_config(768, tiangong_memory::MemoryVectorMode::EmbeddedLanceDb);
        let config = CoreConfig::default();
        let summary = memory_config_summary(&config);

        let model = summary.model.as_ref().expect("应包含 memory model 摘要");
        assert_eq!(model.base_url, "http://memory.example");
        assert_eq!(model.model, "memory-model");
        let embedding = summary.embedding.as_ref().expect("应包含 embedding 摘要");
        assert_eq!(embedding.base_url, "http://embed.example");
        assert_eq!(embedding.model, "embed-model");
        assert_eq!(embedding.dimension, 768);
        let rerank = summary.rerank.as_ref().expect("应包含 rerank 摘要");
        assert_eq!(rerank.base_url, "http://rerank.example");
        assert_eq!(rerank.model, "rerank-model");
        assert_eq!(summary.vector_mode, "EmbeddedLanceDb");

        write_memory_test_config(1024, tiangong_memory::MemoryVectorMode::EmbeddedLanceDb);
        let changed_dimension = memory_config_summary(&CoreConfig::default());
        assert!(memory_config_changed(&summary, &changed_dimension));

        write_memory_test_config(768, tiangong_memory::MemoryVectorMode::Disabled);
        let changed_vector_mode = memory_config_summary(&CoreConfig::default());
        assert!(memory_config_changed(&summary, &changed_vector_mode));

        write_memory_test_config(768, tiangong_memory::MemoryVectorMode::EmbeddedLanceDb);
        let same_memory_config = memory_config_summary(&CoreConfig::default());
        assert!(!memory_config_changed(&summary, &same_memory_config));
        assert!(memory_config_can_update_in_place(
            &summary,
            &changed_dimension
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_reuses_global_handle_regardless_of_workspace() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let config = CoreConfig::default();

        let handle_a = get_or_init_memory_async(&config, 1, tiangong_memory::ProcessType::Cli)
            .await
            .expect("memory handle 应启动成功");
        let handle_a_again =
            get_or_init_memory_async(&config, 1, tiangong_memory::ProcessType::Cli)
                .await
                .expect("memory handle 应可复用");
        let handle_b = get_or_init_memory_async(&config, 1, tiangong_memory::ProcessType::Cli)
            .await
            .expect("memory handle 应启动成功");

        assert!(
            handle_a.is_same_handle(&handle_a_again),
            "相同参数应复用同一个 MemoryHandle"
        );
        assert!(
            handle_a.is_same_handle(&handle_b),
            "全局模式下应复用同一个 MemoryHandle"
        );

        shutdown_memory_registry_blocking();
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_async_path_starts_or_connects_inside_runtime() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let config = CoreConfig::default();
        let handle = get_or_init_memory_async(&config, 1, tiangong_memory::ProcessType::Gui)
            .await
            .expect("tokio runtime 内应能初始化 MemoryHandle");
        let same_handle = get_or_init_memory_async(&config, 1, tiangong_memory::ProcessType::Gui)
            .await
            .expect("tokio runtime 内应能复用 MemoryHandle");

        assert!(
            handle.is_same_handle(&same_handle),
            "异步路径应复用同一个 MemoryHandle"
        );

        shutdown_memory_registry_blocking();
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_hot_updates_memory_config_in_place() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let initial = CoreConfig::default();
        let initial_handle =
            get_or_init_memory_async(&initial, 1, tiangong_memory::ProcessType::Cli)
                .await
                .expect("初始 MemoryHandle 应启动成功");

        write_memory_test_config(1024, tiangong_memory::MemoryVectorMode::EmbeddedLanceDb);
        let updated = CoreConfig::default();
        let expected_summary = memory_config_summary(&updated);
        let updated_handle =
            get_or_init_memory_async(&updated, 2, tiangong_memory::ProcessType::Cli)
                .await
                .expect("MemoryHandle 应支持热更新后继续可用");

        assert!(
            initial_handle.is_same_handle(&updated_handle),
            "Memory 配置热更新应原地复用同一 handle"
        );

        let slot = MEMORY_HANDLE.get().expect("registry 应已初始化");
        let guard = slot.lock().expect("registry 锁应可用");
        let entry = guard.as_ref().expect("entry 应存在");
        assert_eq!(entry.config_generation, 2);
        assert!(!entry.restart_required);
        assert_eq!(entry.config_summary, expected_summary);
        drop(guard);

        shutdown_memory_registry_blocking();
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_reacts_to_core_config_provider_hot_reload() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let provider = CoreConfigProvider::new(CoreConfig::default());
        let initial_snapshot = provider.snapshot();
        let initial_handle = get_or_init_memory_async(
            &initial_snapshot,
            provider.generation(),
            tiangong_memory::ProcessType::Cli,
        )
        .await
        .expect("初始 MemoryHandle 应启动成功");

        write_memory_test_config(2048, tiangong_memory::MemoryVectorMode::EmbeddedLanceDb);
        provider.update(|config| {
            config.context_limit += 1;
        });
        let updated_snapshot = provider.snapshot();
        let expected_summary = memory_config_summary(&updated_snapshot);
        let updated_handle = get_or_init_memory_async(
            &updated_snapshot,
            provider.generation(),
            tiangong_memory::ProcessType::Cli,
        )
        .await
        .expect("配置热重载后 MemoryHandle 应继续可用");

        assert!(
            initial_handle.is_same_handle(&updated_handle),
            "CoreConfigProvider 热重载后应原地复用同一 MemoryHandle"
        );

        let slot = MEMORY_HANDLE.get().expect("registry 应已初始化");
        let guard = slot.lock().expect("registry 锁应可用");
        let entry = guard.as_ref().expect("entry 应存在");
        assert_eq!(
            entry.config_generation,
            provider.generation(),
            "registry 应记录最新配置 generation"
        );
        assert_eq!(
            entry.config_summary, expected_summary,
            "registry 应记录热重载后的 Memory 配置摘要"
        );
        assert!(
            !entry.restart_required,
            "model/embedding/dimension/vector_mode 变化应通过原地热更新完成"
        );
        drop(guard);

        shutdown_memory_registry_blocking();
    }

    fn write_memory_test_config(dimension: usize, vector_mode: tiangong_memory::MemoryVectorMode) {
        let config = tiangong_memory::MemoryConfig {
            model: Some(tiangong_memory::MemoryLlmConfig {
                base_url: "http://memory.example".to_string(),
                api_key: "secret".to_string(),
                model: "memory-model".to_string(),
                ..Default::default()
            }),
            embedding: Some(tiangong_memory::MemoryEmbeddingConfig {
                base_url: "http://embed.example".to_string(),
                api_key: "secret".to_string(),
                model: "embed-model".to_string(),
                dimension,
                ..Default::default()
            }),
            rerank: Some(tiangong_memory::MemoryRerankConfig {
                base_url: "http://rerank.example".to_string(),
                api_key: "secret".to_string(),
                model: "rerank-model".to_string(),
                ..Default::default()
            }),
            vector_mode,
        };
        config.save().expect("写入 Memory 测试配置失败");
    }

    struct MemoryRegistryEnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_userprofile: Option<std::ffi::OsString>,
        home: std::path::PathBuf,
    }

    impl MemoryRegistryEnvGuard {
        fn enter() -> Self {
            let prev_home = std::env::var_os("HOME");
            let prev_userprofile = std::env::var_os("USERPROFILE");
            let home =
                std::env::temp_dir().join(format!("tiangong-core-memory-{}", scru128::new()));
            std::fs::create_dir_all(&home).expect("创建 memory registry 测试目录失败");
            unsafe {
                std::env::set_var("HOME", &home);
                std::env::set_var("USERPROFILE", &home);
            }
            Self {
                prev_home,
                prev_userprofile,
                home,
            }
        }
    }

    impl Drop for MemoryRegistryEnvGuard {
        fn drop(&mut self) {
            shutdown_memory_registry_blocking();
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
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}
