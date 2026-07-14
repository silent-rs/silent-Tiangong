//! 进程级共享 tokio runtime。
//!
//! 所有 TiangongCore 的 worker task 与转发线程都跑在这个共享 runtime 上，
//! 取代此前「每个 Core 一个 OS 线程 + 一个 1-worker multi-thread runtime」的模型。
//!
//! 空闲会话的 worker task 停在 `cmd_rx.recv().await`，future 被 park、线程归还
//! runtime 池，实现「空闲零线程占用」。
//!
//! runtime 必须是 multi-thread：LLM crate 的 `provider_client` 内部使用
//! `tokio::task::block_in_place`（仅在 multi-thread runtime 可用）。

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// 共享 runtime 的 worker 线程数。
///
/// 2 个 worker 线程足以支撑桌面应用的并发 turn + LLM 流式请求；空闲会话的
/// worker task 被 park，不占线程。
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
