//! 宿主权威工作区（RFC 0017 透明执行封套，活跃单值模型）。
//!
//! 会话就绪与每个 turn 开始时刷新"当前活跃工作区"；沙箱策略的工作区
//! 校验只信任活跃值——请求负载中的 `cwd` / `access.workspace` 仅作候选，
//! 必须与活跃工作区（或其子目录）匹配。
//!
//! 审查修订说明：此前为全局合集（任一历史会话登记过即放行），存在
//! 跨会话声明攻击面；收敛为单值后，攻击面缩小为"并发活跃的另一会话"
//! ——完整的 session_id 绑定需要调用上下文链路改造，见 RFC 开放问题。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// 活跃窗口：超过该时长未刷新的工作区不再信任子目录声明
/// （超时后仅精确匹配仍可能通过 canonicalize 相等性判断）。
const FRESH_WINDOW_SECS: u64 = 600;

struct ActiveWorkspace {
    path: PathBuf,
    refreshed_at: Instant,
}

fn active() -> &'static Mutex<Option<ActiveWorkspace>> {
    static ACTIVE: std::sync::OnceLock<Mutex<Option<ActiveWorkspace>>> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// 规范化路径：直接 canonicalize；目标不存在时规范化最近的存在祖先
/// 再拼回余下部分（处理 macOS `/var` → `/private/var` 等别名差异）。
fn canonical(path: &Path) -> PathBuf {
    if let Ok(full) = std::fs::canonicalize(path) {
        return full;
    }
    // ancestors 原序为自身 → 根：第一个存在的即最深的可解析祖先。
    for (index, ancestor) in path.ancestors().enumerate() {
        if let Ok(real) = std::fs::canonicalize(ancestor) {
            let mut result = real;
            for component in path.ancestors().take(index).collect::<Vec<_>>() {
                if let Some(name) = component.file_name() {
                    result = result.join(name);
                }
            }
            return result;
        }
    }
    path.to_path_buf()
}

/// 刷新活跃工作区（会话就绪 / turn 开始时调用；多会话时后写者生效）。
pub fn register(path: &Path) {
    if path.as_os_str().is_empty() || !path.is_dir() {
        return;
    }
    if let Ok(mut slot) = active().lock() {
        *slot = Some(ActiveWorkspace {
            path: canonical(path),
            refreshed_at: Instant::now(),
        });
    }
}

/// 判定候选路径是否为（或位于）当前活跃工作区内。
///
/// 不信任陈旧值：活跃工作区超过窗口未刷新时，仅接受与已登记路径
/// canonicalize 后精确相等的候选（子目录声明失效）。
pub fn is_authoritative(candidate: &Path) -> bool {
    let candidate = canonical(candidate);
    let Ok(slot) = active().lock() else {
        return false;
    };
    let Some(workspace) = slot.as_ref() else {
        return false;
    };
    if workspace.refreshed_at.elapsed().as_secs() < FRESH_WINDOW_SECS {
        candidate.starts_with(&workspace.path)
    } else {
        candidate == workspace.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_workspace_trust_semantics() {
        // 单值模型为进程级状态：用例合并串行断言，避免并行覆盖。
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        // 登记后：工作区与其子目录受信，其它路径拒绝。
        register(first.path());
        assert!(is_authoritative(first.path()));
        assert!(is_authoritative(&first.path().join("sub/dir")));
        assert!(!is_authoritative(outside.path()));
        assert!(!is_authoritative(Path::new("/etc")));

        // 后写者生效：换到第二工作区后，第一工作区（含其曾受信子目录）
        // 不再受信——跨会话声明攻击面收敛。
        register(second.path());
        assert!(!is_authoritative(first.path()));
        assert!(!is_authoritative(&first.path().join("sub/dir")));
        assert!(is_authoritative(second.path()));
    }
}
