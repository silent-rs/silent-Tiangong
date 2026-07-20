//! Workspace：会话级工作目录的轻量解析。
//!
//! 当前实现只是「全局 workspace_dir + 会话 cwd_mode」的解析（与 app-state
//! 现有 `SessionState.workspace_dir` 语义一致）。issue #245 把 workspace 概念
//! 收敛到此处，后续若引入「多 workspace / workspace 配置文件」在此扩展，
//! 不再回流到 app-state 的 Session 列表。
//!
//! 注意：本模块不持有状态，只做纯解析；真相源仍是磁盘 session 文件的 `cwd` 字段。

/// 解析某会话实际生效的工作目录。
///
/// - `Isolated` / `Custom`：使用会话自身的 `cwd`
/// - `Inherit`：使用全局 `inherit_workspace_dir`（桌面默认 = 进程启动目录）
pub fn resolve_effective_cwd(
    session_cwd: &str,
    session_cwd_mode: tiangong_core::session::SessionCwdMode,
    inherit_workspace_dir: &str,
) -> String {
    use tiangong_core::session::SessionCwdMode;
    match session_cwd_mode {
        SessionCwdMode::Inherit => {
            if session_cwd.is_empty() {
                inherit_workspace_dir.to_string()
            } else {
                session_cwd.to_string()
            }
        }
        SessionCwdMode::Isolated | SessionCwdMode::Custom => session_cwd.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::session::SessionCwdMode;

    #[test]
    fn inherit_uses_global_when_empty() {
        assert_eq!(
            resolve_effective_cwd("", SessionCwdMode::Inherit, "/global"),
            "/global"
        );
    }

    #[test]
    fn inherit_uses_session_when_set() {
        assert_eq!(
            resolve_effective_cwd("/sess", SessionCwdMode::Inherit, "/global"),
            "/sess"
        );
    }

    #[test]
    fn isolated_always_uses_session() {
        assert_eq!(
            resolve_effective_cwd("/iso", SessionCwdMode::Isolated, "/global"),
            "/iso"
        );
    }
}
