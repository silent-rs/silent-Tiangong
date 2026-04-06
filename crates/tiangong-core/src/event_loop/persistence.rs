//! EventLoop 状态持久化
//!
//! 将 LoopState 写入 `~/.tiangong/loops/{session_id}.json`，
//! 支持优雅关闭时保存和启动时恢复。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;
use crate::session::Message;

use super::types::{LoopPhase, LoopState, PendingToolCall};

/// 持久化目录名
const LOOPS_DIR: &str = "loops";

/// 持久化的 LoopState（可序列化版本）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedLoopState {
    /// 会话 ID
    pub session_id: String,
    /// 循环上下文
    pub loop_context: Vec<Message>,
    /// 累积 token 使用量
    pub accumulated_usage: TokenUsage,
    /// 当前轮次
    pub round: usize,
    /// 待处理的工具调用
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// 被中断时的阶段
    pub interrupted_phase: LoopPhase,
    /// 持久化时间
    pub persisted_at: String,
}

impl From<&LoopState> for PersistedLoopState {
    fn from(state: &LoopState) -> Self {
        Self {
            session_id: state.session_id.clone(),
            loop_context: state.loop_context.clone(),
            accumulated_usage: state.accumulated_usage.clone(),
            round: state.round,
            pending_tool_calls: state.pending_tool_calls.clone(),
            interrupted_phase: state.interrupted_phase.clone(),
            persisted_at: chrono::Local::now().naive_local().to_string(),
        }
    }
}

impl From<PersistedLoopState> for LoopState {
    fn from(p: PersistedLoopState) -> Self {
        Self {
            session_id: p.session_id,
            loop_context: p.loop_context,
            accumulated_usage: p.accumulated_usage,
            round: p.round,
            pending_tool_calls: p.pending_tool_calls,
            interrupted_phase: p.interrupted_phase,
            suspended_at: Some(p.persisted_at),
        }
    }
}

/// 获取 loops 持久化目录
pub fn loops_dir() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join(LOOPS_DIR)
}

/// 保存 LoopState 到磁盘
pub fn save_loop_state(state: &LoopState) -> Result<()> {
    let dir = loops_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("创建 loops 目录失败：{}", dir.display()))?;

    let persisted = PersistedLoopState::from(state);
    let path = dir.join(format!("{}.json", state.session_id));
    let content = serde_json::to_string_pretty(&persisted)
        .with_context(|| format!("序列化 LoopState 失败：{}", state.session_id))?;

    fs::write(&path, content)
        .with_context(|| format!("写入 LoopState 失败：{}", path.display()))?;

    tracing::debug!(session_id = %state.session_id, "LoopState 已持久化");
    Ok(())
}

/// 加载所有持久化的 LoopState
pub fn load_all_loop_states() -> Result<Vec<LoopState>> {
    let dir = loops_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("读取 loops 目录失败：{}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            match fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<PersistedLoopState>(&content) {
                    Ok(persisted) => {
                        let mut state = LoopState::from(persisted);
                        // Running 状态被中断 → 标记待处理工具调用为失败
                        if state.interrupted_phase == LoopPhase::Processing
                            && !state.pending_tool_calls.is_empty()
                        {
                            let failed_tools: Vec<String> = state
                                .pending_tool_calls
                                .iter()
                                .map(|c| c.name.clone())
                                .collect();
                            state.loop_context.push(crate::session::Message {
                                id: scru128::new().to_string(),
                                role: crate::session::MessageRole::System,
                                content: format!(
                                    "上次运行被中断，以下工具调用未完成：{}",
                                    failed_tools.join(", ")
                                ),
                                reasoning_content: String::new(),
                                worker_id: None,
                                created_at: crate::session::now_text(),
                            });
                            state.pending_tool_calls.clear();
                        }
                        states.push(state);
                    }
                    Err(err) => {
                        tracing::warn!("解析 LoopState 失败 {}：{err}", path.display());
                    }
                },
                Err(err) => {
                    tracing::warn!("读取 LoopState 失败 {}：{err}", path.display());
                }
            }
        }
    }

    if !states.is_empty() {
        tracing::info!("启动恢复：加载了 {} 个 LoopState", states.len());
    }
    Ok(states)
}

/// 删除指定会话的持久化文件
pub fn remove_loop_state(session_id: &str) -> Result<()> {
    let path = loops_dir().join(format!("{session_id}.json"));
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("删除 LoopState 失败：{}", path.display()))?;
    }
    Ok(())
}

/// 清理所有持久化文件
pub fn cleanup_all_loop_states() -> Result<usize> {
    let dir = loops_dir();
    if !dir.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            let _ = fs::remove_file(entry.path());
            count += 1;
        }
    }
    Ok(count)
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_roundtrip() {
        let state = LoopState::new("test-session".into());
        let persisted = PersistedLoopState::from(&state);
        let json = serde_json::to_string(&persisted).unwrap();
        let parsed: PersistedLoopState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "test-session");

        let restored = LoopState::from(parsed);
        assert_eq!(restored.session_id, "test-session");
        assert!(restored.suspended_at.is_some());
    }

    #[test]
    fn loops_dir_path() {
        let dir = loops_dir();
        assert!(dir.to_string_lossy().contains(".tiangong"));
        assert!(dir.to_string_lossy().contains("loops"));
    }
}
