//! 任务状态持久化
//!
//! 将任务状态实时写入 `~/.tiangong/tasks/{task_id}.json`，
//! 启动时可扫描恢复未完成任务。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::model::UnifiedTask;

/// 任务持久化目录名
const TASKS_DIR: &str = "tasks";

/// 获取任务持久化目录路径
pub fn tasks_dir() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join(TASKS_DIR)
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

/// 保存任务到磁盘
pub fn save_task(task: &UnifiedTask) -> Result<()> {
    let dir = tasks_dir();
    fs::create_dir_all(&dir).with_context(|| format!("创建任务目录失败：{}", dir.display()))?;

    let path = dir.join(format!("{}.json", task.id));
    let content = serde_json::to_string_pretty(task)
        .with_context(|| format!("序列化任务失败：{}", task.id))?;

    fs::write(&path, content).with_context(|| format!("写入任务文件失败：{}", path.display()))?;

    Ok(())
}

/// 加载单个任务
pub fn load_task(task_id: &str) -> Result<UnifiedTask> {
    let path = tasks_dir().join(format!("{task_id}.json"));
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取任务文件失败：{}", path.display()))?;
    let task: UnifiedTask = serde_json::from_str(&content)
        .with_context(|| format!("解析任务文件失败：{}", path.display()))?;
    Ok(task)
}

/// 加载所有持久化的任务
pub fn load_all_tasks() -> Result<Vec<UnifiedTask>> {
    let dir = tasks_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut tasks = Vec::new();
    for entry in
        fs::read_dir(&dir).with_context(|| format!("读取任务目录失败：{}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            match load_task_from_path(&path) {
                Ok(task) => tasks.push(task),
                Err(err) => {
                    tracing::warn!("加载任务文件失败 {}：{err}", path.display());
                }
            }
        }
    }

    Ok(tasks)
}

/// 删除任务持久化文件
pub fn remove_task(task_id: &str) -> Result<()> {
    let path = tasks_dir().join(format!("{task_id}.json"));
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("删除任务文件失败：{}", path.display()))?;
    }
    Ok(())
}

/// 清理所有已完成任务的持久化文件
pub fn cleanup_completed_tasks() -> Result<usize> {
    let tasks = load_all_tasks()?;
    let mut count = 0;
    for task in &tasks {
        if task.status.is_terminal() {
            remove_task(&task.id)?;
            count += 1;
        }
    }
    Ok(count)
}

fn load_task_from_path(path: &Path) -> Result<UnifiedTask> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取任务文件失败：{}", path.display()))?;
    let task: UnifiedTask = serde_json::from_str(&content)
        .with_context(|| format!("解析任务文件失败：{}", path.display()))?;
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::model::{TaskKind, UnifiedTask, UnifiedTaskStatus};

    #[test]
    fn task_roundtrip_serialization() {
        let mut task = UnifiedTask::new(
            TaskKind::BackgroundCommand,
            "cargo build".into(),
            "bg".into(),
        );
        task.set_status(UnifiedTaskStatus::Running);
        task.session_id = Some("s1".into());
        task.command = Some("cargo build --release".into());

        let json = serde_json::to_string_pretty(&task).unwrap();
        let loaded: UnifiedTask = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.id, task.id);
        assert_eq!(loaded.status, UnifiedTaskStatus::Running);
        assert_eq!(loaded.session_id, Some("s1".into()));
        assert_eq!(loaded.command, Some("cargo build --release".into()));
    }

    #[test]
    fn tasks_dir_path() {
        let dir = tasks_dir();
        assert!(dir.to_string_lossy().contains(".tiangong"));
        assert!(dir.to_string_lossy().contains("tasks"));
    }
}
