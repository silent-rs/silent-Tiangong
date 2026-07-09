//! Memory Handle 全局单例：初始化、热更新、关闭

use std::sync::{Mutex, OnceLock};

#[cfg(test)]
use tiangong_core::core_config::CoreConfig;
use tiangong_types::now_text;

use crate::{MemoryConfig, MemoryOptions};

static MEMORY_HANDLE: OnceLock<Mutex<Option<MemoryEntry>>> = OnceLock::new();
static MEMORY_INIT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

struct MemoryEntry {
    managed: crate::ManagedMemory,
    handle: crate::MemoryHandle,
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

/// 获取或初始化全局 Memory Handle（异步）。
pub async fn get_or_init_memory_async(
    options: MemoryOptions,
    config_generation: u64,
    process_type: crate::ProcessType,
) -> Option<crate::MemoryHandle> {
    // 运行时禁用检查：`tiangong memory disable` 会创建标记文件。
    // 标记存在时跳过 Memory 启动，全局生效（CLI/Server/Desktop）。
    if crate::is_memory_disabled() {
        tracing::info!("Memory 已被禁用标记关闭（memory/.disabled），跳过启动");
        return None;
    }

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

    match crate::start_or_connect_with_options(options, process_type).await {
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
pub async fn get_or_init_memory_handle_async() -> Option<crate::MemoryHandle> {
    let options = MemoryConfig::load_or_default().to_options();
    get_or_init_memory_async(options, 0, crate::ProcessType::Gui).await
}

/// 为指定进程类型初始化 Memory Handle（供 GUI / CLI / Server 入口层创建插件前调用）。
///
/// 内部经全局 registry 复用同一 actor 单例，config 变化时自动 reconfigure。
/// 返回的 `MemoryHandle`（内部 Arc）传入 `MemoryPlugin::new` 构造注入。
pub async fn init_memory_handle_for_process(
    config_generation: u64,
    process_type: crate::ProcessType,
) -> Option<crate::MemoryHandle> {
    let options = MemoryConfig::load_or_default().to_options();
    get_or_init_memory_async(options, config_generation, process_type).await
}

/// 通过 Core 读取 Memory 独立配置，供 GUI 配置页展示。
pub fn load_memory_config() -> MemoryConfig {
    MemoryConfig::load_or_default()
}

/// 通过 Core 保存 Memory 独立配置，避免 GUI 入口直接操作 Memory 配置文件。
pub fn save_memory_config(config: MemoryConfig) -> anyhow::Result<()> {
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

fn memory_config_summary_from_options(options: &crate::MemoryOptions) -> MemoryConfigSummary {
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
    use serial_test::serial;

    static MEMORY_REGISTRY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn memory_config_summary(_config: &CoreConfig) -> MemoryConfigSummary {
        let options = crate::MemoryConfig::load_or_default().to_options();
        memory_config_summary_from_options(&options)
    }

    fn memory_registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
        MEMORY_REGISTRY_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("memory registry 测试锁已污染")
    }

    #[test]
    #[serial]
    fn memory_config_summary_tracks_memory_relevant_fields() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        write_memory_test_config(768, crate::MemoryVectorMode::EmbeddedLanceDb);
        let summary = memory_config_summary(&Default::default());

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

        write_memory_test_config(1024, crate::MemoryVectorMode::EmbeddedLanceDb);
        write_memory_test_config(1024, crate::MemoryVectorMode::EmbeddedLanceDb);
        let changed_dimension = memory_config_summary(&Default::default());
        assert!(memory_config_changed(&summary, &changed_dimension));

        write_memory_test_config(768, crate::MemoryVectorMode::Disabled);
        write_memory_test_config(768, crate::MemoryVectorMode::Disabled);
        let changed_vector_mode = memory_config_summary(&Default::default());
        assert!(memory_config_changed(&summary, &changed_vector_mode));

        write_memory_test_config(768, crate::MemoryVectorMode::EmbeddedLanceDb);
        write_memory_test_config(768, crate::MemoryVectorMode::EmbeddedLanceDb);
        let same_memory_config = memory_config_summary(&Default::default());
        assert!(!memory_config_changed(&summary, &same_memory_config));
        assert!(memory_config_can_update_in_place(
            &summary,
            &changed_dimension
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_reuses_global_handle_regardless_of_workspace() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter_without_vector();
        shutdown_memory_registry_blocking();

        let options = MemoryConfig::load_or_default().to_options();
        let handle_a = get_or_init_memory_async(options.clone(), 1, crate::ProcessType::Cli)
            .await
            .expect("memory handle 应启动成功");
        let handle_a_again = get_or_init_memory_async(options.clone(), 1, crate::ProcessType::Cli)
            .await
            .expect("memory handle 应可复用");
        let handle_b = get_or_init_memory_async(options, 1, crate::ProcessType::Cli)
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
    #[serial]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_async_path_starts_or_connects_inside_runtime() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter_without_vector();
        shutdown_memory_registry_blocking();

        let options = MemoryConfig::load_or_default().to_options();
        let handle = get_or_init_memory_async(options.clone(), 1, crate::ProcessType::Gui)
            .await
            .expect("tokio runtime 内应能初始化 MemoryHandle");
        let same_handle = get_or_init_memory_async(options, 1, crate::ProcessType::Gui)
            .await
            .expect("tokio runtime 内应能复用 MemoryHandle");

        assert!(
            handle.is_same_handle(&same_handle),
            "异步路径应复用同一个 MemoryHandle"
        );

        shutdown_memory_registry_blocking();
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_hot_updates_memory_config_in_place() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter_without_vector();
        shutdown_memory_registry_blocking();

        let initial_options = MemoryConfig::load_or_default().to_options();
        let initial_handle = get_or_init_memory_async(initial_options, 1, crate::ProcessType::Cli)
            .await
            .expect("初始 MemoryHandle 应启动成功");

        write_memory_test_config(1024, crate::MemoryVectorMode::Disabled);
        let expected_summary = memory_config_summary(&Default::default());
        let updated_options = MemoryConfig::load_or_default().to_options();
        let updated_handle = get_or_init_memory_async(updated_options, 2, crate::ProcessType::Cli)
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
    #[serial]
    #[allow(clippy::await_holding_lock)]
    async fn memory_registry_reacts_to_core_config_provider_hot_reload() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter_without_vector();
        shutdown_memory_registry_blocking();

        let initial_options = MemoryConfig::load_or_default().to_options();
        let initial_handle =
            get_or_init_memory_async(initial_options.clone(), 1, crate::ProcessType::Cli)
                .await
                .expect("初始 MemoryHandle 应启动成功");

        write_memory_test_config(2048, crate::MemoryVectorMode::Disabled);
        let expected_summary = memory_config_summary(&Default::default());
        let updated_handle = get_or_init_memory_async(
            MemoryConfig::load_or_default().to_options(),
            2,
            crate::ProcessType::Cli,
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
            entry.config_generation, 2,
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

    fn write_memory_test_config(dimension: usize, vector_mode: crate::MemoryVectorMode) {
        let config = crate::MemoryConfig {
            model: Some(crate::MemoryLlmConfig {
                base_url: "http://memory.example".to_string(),
                api_key: "secret".to_string(),
                model: "memory-model".to_string(),
                ..Default::default()
            }),
            embedding: Some(crate::MemoryEmbeddingConfig {
                base_url: "http://embed.example".to_string(),
                api_key: "secret".to_string(),
                model: "embed-model".to_string(),
                dimension,
                ..Default::default()
            }),
            rerank: Some(crate::MemoryRerankConfig {
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

        /// 进入隔离环境并写入「向量层禁用」的 Memory 配置。
        ///
        /// 这些测试只验证 registry 的句柄复用/热更新逻辑，不涉及向量检索。
        /// 用 `Disabled` 跳过 lancedb（C++ FFI）初始化，从根上消除测试进程 teardown
        /// 时 Actor 线程析构 lancedb 与进程 C++ 静态析构的竞态（Linux 偶发 SIGSEGV）。
        fn enter_without_vector() -> Self {
            let guard = Self::enter();
            write_memory_test_config(768, crate::MemoryVectorMode::Disabled);
            guard
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
