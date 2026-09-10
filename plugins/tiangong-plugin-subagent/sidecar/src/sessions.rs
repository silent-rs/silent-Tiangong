//! 天工会话文件访问：列表扫描与内容提取（只读，Value 层解析）。
//!
//! 会话真相源是 `~/.tiangong/sessions/<id>.json`（宿主 Session 的 serde
//! 序列化）。sidecar 只读不动：列表给管理页选择器，内容给记忆整理。

use anyhow::{Context, Result, bail};
use serde_json::Value;

use tiangong_plugin_subagent_protocol::ops::SessionBrief;

use crate::paths::storage_root;

fn sessions_dir() -> Result<std::path::PathBuf> {
    Ok(storage_root()?.join("sessions"))
}

/// 校验会话 ID 形态（scru128 单段）。
fn validate_session_id(session_id: &str) -> Result<()> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid {
        Ok(())
    } else {
        bail!("会话 ID 无效: {session_id:?}")
    }
}

/// 读取会话 JSON（Value 形态）。
pub fn load_session_json(session_id: &str) -> Result<Value> {
    validate_session_id(session_id)?;
    let path = sessions_dir()?.join(format!("{session_id}.json"));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("会话文件不存在或不可读: {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("解析会话文件失败: {session_id}"))
}

/// 扫描会话列表（按更新时间倒序）。
pub fn list_sessions() -> Vec<SessionBrief> {
    let mut sessions = Vec::new();
    let Ok(dir) = sessions_dir() else {
        return sessions;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        sessions.push(SessionBrief {
            id: id.to_string(),
            title: value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("（无标题）")
                .to_string(),
            updated_at: value
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            message_count: value
                .get("messages")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        });
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

/// 会话是否存在。
/// 读取会话的工作区（cwd）：协作运行的 workspace 域以发起会话为准，
/// 不随「最近激活」漂移到其他工作区。
pub fn session_workspace(session_id: &str) -> Option<String> {
    let session = load_session_json(session_id).ok()?;
    session
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string)
        .filter(|cwd| std::path::Path::new(cwd).is_dir())
}

// 工作区对齐已改为随投递消息传递（delivery::deliver_message 的 workspace
// 参数），由服务端在会话创建/更新时统一落位——sidecar 不得直改会话文件
// （会与宿主保存竞争，且首次投递时会话尚未创建、直改必然落空）。

pub fn session_exists(session_id: &str) -> bool {
    validate_session_id(session_id).is_ok()
        && sessions_dir()
            .map(|dir| dir.join(format!("{session_id}.json")).is_file())
            .unwrap_or(false)
}
