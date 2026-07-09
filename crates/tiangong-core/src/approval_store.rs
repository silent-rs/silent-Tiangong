//! 待审批请求独立存储
//!
//! 审批状态是运行时瞬态数据，独立于 session 存储到 `~/.tiangong/pending-approvals.json`。
//! 按 session_id 索引，支持多会话同时存在待审批请求。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::PendingApproval;

/// 全局待审批存储（session_id → approvals）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ApprovalStoreData {
    sessions: HashMap<String, Vec<PendingApproval>>,
}

fn store_path() -> PathBuf {
    crate::storage::storage_root().join("pending-approvals.json")
}

fn load_store() -> ApprovalStoreData {
    let path = store_path();
    if !path.is_file() {
        return ApprovalStoreData::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_store(data: &ApprovalStoreData) {
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(data)
        && let Err(err) = std::fs::write(&path, content)
    {
        tracing::warn!(error = %err, "审批存储写入失败");
    }
}

/// 添加待审批请求
pub fn add_pending(session_id: &str, approval: PendingApproval) {
    let mut data = load_store();
    data.sessions
        .entry(session_id.to_string())
        .or_default()
        .push(approval);
    save_store(&data);
}

/// 移除指定审批请求
pub fn remove_pending(session_id: &str, request_id: &str) {
    let mut data = load_store();
    if let Some(approvals) = data.sessions.get_mut(session_id) {
        approvals.retain(|a| a.request_id != request_id);
        if approvals.is_empty() {
            data.sessions.remove(session_id);
        }
    }
    save_store(&data);
}

/// 获取指定会话的待审批列表
pub fn get_pending(session_id: &str) -> Vec<PendingApproval> {
    let data = load_store();
    data.sessions.get(session_id).cloned().unwrap_or_default()
}

/// 清空指定会话的所有待审批
pub fn clear_pending(session_id: &str) {
    let mut data = load_store();
    if data.sessions.remove(session_id).is_some() {
        save_store(&data);
    }
}
