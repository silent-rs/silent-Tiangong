//! 进程级共享 tokio runtime。
//!
//! 所有 TiangongCore 的 Agent driver 都跑在这个共享 runtime 上。
//! Agent 调度（Inbox、driver、唤醒与关闭）见 [`crate::react::inbox`]。
//!
//! runtime 必须是 multi-thread：LLM crate 的 `provider_client` 内部使用
//! `tokio::task::block_in_place`（仅在 multi-thread runtime 可用）。

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// 共享 runtime 的 worker 线程数。
const WORKER_THREADS: usize = 2;

static SHARED_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// 获取进程级共享 tokio runtime。
pub fn shared_runtime() -> &'static Runtime {
    SHARED_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("创建共享 tokio runtime 失败")
    })
}
