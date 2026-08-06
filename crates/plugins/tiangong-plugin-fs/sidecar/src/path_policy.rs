//! 路径解析策略（沙箱预留点 A）。
//!
//! 当前 fs 的路径校验散落在 toolkit 的自由函数里（`resolve_workspace_path_with`
//! / `resolve_write_path_from_base` 等），且 fs/index/fetch 三处各自复制同一套
//! `if full_trust` 分支。本模块把它收敛成一个可替换策略 trait——当前唯一实现
//! `TrustModePathPolicy` 包 toolkit 调用；未来引入沙箱（landlock、路径白名单、
//! overlay 只读映射等）时，只需提供新的 `PathPolicy` 实现，业务代码（各 handler）
//! 不动。
//!
//! 这层抽象也为未来 runtime 层的进程级沙箱（对齐 PLAN.md「权限探测、签名体系」
//! 排期）留出对接面：当 runtime 能下发沙箱配置时，`PathPolicy` 实现可接收该
//! 配置，把"允许的根目录/拒绝模式"应用到路径解析上。

use std::path::{Path, PathBuf};

use anyhow::Result;
use tiangong_toolkit as shared;

/// 路径解析策略：把"当前会话的访问能力 + 原始路径"解析为可读/可写的绝对路径。
///
/// 实现方负责越界校验、符号链接解析、`..` 消除等。当前唯一实现是
/// [`TrustModePathPolicy`]；未来沙箱实现可替换本 trait。
pub trait PathPolicy: Send + Sync {
    /// 读路径解析（list_dir / read_file / tree_dir 用）。
    fn resolve_read(&self, raw: &str, base: &Path) -> Result<PathBuf>;

    /// 写路径解析（write_file / replace_in_file / apply_patch 用）。
    fn resolve_write(&self, raw: &str, base: &Path) -> Result<PathBuf>;
}

/// 基于信任模式的路径策略（当前唯一实现，对齐原进程内 fs 插件语义）。
///
/// - `full_trust` 为 true：读路径放宽（可读工作区外、不存在不报错）；
///   写路径**仍受** `resolve_write_path_from_base` 的 allowed roots 约束
///   （workspace + `~/.tiangong/`），这是与原实现一致的语义。
/// - `full_trust` 为 false：读路径强制 canonicalize（不存在即失败），
///   写路径同样走 allowed roots 校验。
pub struct TrustModePathPolicy {
    full_trust: bool,
}

impl TrustModePathPolicy {
    pub fn new(full_trust: bool) -> Self {
        Self { full_trust }
    }
}

impl PathPolicy for TrustModePathPolicy {
    fn resolve_read(&self, raw: &str, base: &Path) -> Result<PathBuf> {
        if self.full_trust {
            shared::resolve_workspace_path_trusted_with(raw, base)
        } else {
            shared::resolve_workspace_path_with(raw, base)
        }
    }

    fn resolve_write(&self, raw: &str, base: &Path) -> Result<PathBuf> {
        // 信任模式仍然限制写入范围（与原 fs 实现一致）。
        shared::resolve_write_path_from_base(raw, base)
    }
}
