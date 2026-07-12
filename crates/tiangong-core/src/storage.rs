//! 存储根目录注入点
//!
//! core 内所有持久化点（audit / custom_prompt / models_config / approval_store /
//! session / core_config）都通过本模块取存储根目录（`~/.tiangong`）。
//!
//! ## 所有权与注入方向
//!
//! **路径计算归 app 层**（`tiangong-app-state`），core 不做任何环境变量计算，
//! 也不对外提供 `~/.tiangong` 的计算入口。app 层在 `TiangongState::load_or_default`
//! 启动时调用 [`set_storage_root`] 把解析好的根目录注入进来；core 的所有持久化点
//! 只读取该值。
//!
//! ## 必须设置
//!
//! [`storage_root`] 在未设置时 **panic**——这是契约的一部分，强制 app 层在启动
//! 序列中先注入。core 自身从不回退到环境变量计算，避免路径来源分散。
//!
//! ## 可重置（测试隔离）
//!
//! 用 `Mutex<Option<PathBuf>>` 而非 `OnceLock`，使单测可按用例重新 set：
//! app-state 的 `with_isolated_state` 改 `HOME` 后构造 `TiangongState`，
//! `load_or_default` 会把对应根目录 set 进来；用例间由进程级串行锁保证不交叉。

use std::path::PathBuf;
use std::sync::Mutex;

static STORAGE_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 注入存储根目录（`~/.tiangong`），由 app 层解析后调用。
///
/// 可重复调用（覆盖前值），供 app 启动与单测隔离使用。
pub fn set_storage_root(root: PathBuf) {
    *STORAGE_ROOT.lock().expect("storage_root mutex poisoned") = Some(root);
}

/// 读取已注入的存储根目录；**未设置时 panic**。
///
/// core 的持久化点都调用本函数。强制"必须先 set"的契约由 panic 体现：
/// 任何未经 app 注入而触达持久化的路径都会立即暴露，而非静默写错位置。
///
/// 仅 core 内部使用（`pub(crate)`）；路径计算归 app 层，外部应通过
/// `tiangong_app_state::app_state::storage_root` 取值，不应直接依赖 core 注入态。
pub(crate) fn storage_root() -> PathBuf {
    STORAGE_ROOT
        .lock()
        .expect("storage_root mutex poisoned")
        .clone()
        .expect("storage_root 未注入：app 层需在启动时调用 set_storage_root")
}
