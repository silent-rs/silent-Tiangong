//! 成员工作区状态：agents/<id>/workspaces/<workspace-id>/ 下的成员自维护
//! 工作状态（plan/context/tasks）。sidecar 只提供安全存取与稳定身份
//! 解析（路径→workspace-id），不理解业务内容——工作含义由成员维护，
//! 执行事实由运行记录维护，两类状态分离。

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::agent_store::AgentStore;
use crate::paths;

/// 工作区状态注入摘要上限（成员规划过长时截断，避免挤占任务正文）。
const STATE_INJECTION_MAX_CHARS: usize = 1500;

/// 解析（或首次登记）workspace 稳定身份：agents/<id>/workspaces/index.json
/// 维护「本机路径 → workspace-id」映射；路径只是位置，id 才是身份。
fn workspace_id(agents: &AgentStore, agent_id: &str, workspace_path: &str) -> Result<String> {
    let dir = workspaces_dir(agents, agent_id)?;
    // 路径归一：符号链接与挂载别名统一到 canonical 形态再登记比较，
    // 避免同一工作区因 /tmp 与 /private/tmp 等差异分裂出多个身份。
    let workspace_path = std::fs::canonicalize(workspace_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| workspace_path.to_string());
    let index_path = dir.join("index.json");
    let mut index: serde_json::Value = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({ "workspaces": [] }));
    let entries = index
        .get_mut("workspaces")
        .and_then(|value| value.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("workspace 索引损坏"))?;
    for entry in entries.iter() {
        if entry.get("path").and_then(|p| p.as_str()) == Some(workspace_path.as_str())
            && let Some(id) = entry.get("id").and_then(|id| id.as_str())
        {
            return Ok(id.to_string());
        }
    }
    // 同目录历史登记（canonicalize 差异）：按已登记 id 的 workspace.json
    // 路径字段反查，命中则补齐新路径别名。
    for entry in entries.iter() {
        let Some(id) = entry.get("id").and_then(|id| id.as_str()) else {
            continue;
        };
        let known = dir.join(id).join("workspace.json");
        if let Ok(meta) = std::fs::read_to_string(&known)
            && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta)
            && meta
                .get("paths")
                .and_then(|p| p.as_array())
                .is_some_and(|paths| {
                    paths
                        .iter()
                        .any(|p| p.as_str() == Some(workspace_path.as_str()))
                })
        {
            return Ok(id.to_string());
        }
    }
    let id = format!("ws-{}", paths::new_id());
    entries.push(serde_json::json!({ "id": id, "path": workspace_path.clone() }));
    paths::atomic_write(&index_path, serde_json::to_string(&index)?.as_bytes())?;
    let ws_dir = dir.join(&id);
    std::fs::create_dir_all(ws_dir.join("tasks"))?;
    let meta = serde_json::json!({
        "workspace_id": id,
        "paths": [workspace_path.clone()],
    });
    paths::atomic_write(
        &ws_dir.join("workspace.json"),
        serde_json::to_string(&meta)?.as_bytes(),
    )?;
    Ok(id)
}

fn workspaces_dir(agents: &AgentStore, agent_id: &str) -> Result<PathBuf> {
    ensure_agent_exists(agents, agent_id)?;
    let dir = agents.root().join(agent_id).join("workspaces");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 公共入口统一校验：成员不存在时不创建任何目录，直接报错。
fn ensure_agent_exists(agents: &AgentStore, agent_id: &str) -> Result<()> {
    if agents.load(agent_id).is_err() {
        bail!("Agent 不存在: {agent_id}");
    }
    Ok(())
}

fn state_dir(agents: &AgentStore, agent_id: &str, workspace_id: &str) -> Result<PathBuf> {
    if !workspace_id.starts_with("ws-") || workspace_id.contains('/') {
        bail!("非法 workspace 身份: {workspace_id}");
    }
    Ok(workspaces_dir(agents, agent_id)?.join(workspace_id))
}

/// 读取成员在某工作区的自维护状态（plan + context + 任务笔记清单）。
pub fn load(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
) -> Result<serde_json::Value> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .map(|body| body.trim().to_string())
            .unwrap_or_default()
    };
    let mut tasks: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("tasks")) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        tasks = names;
    }
    Ok(serde_json::json!({
        "workspace_id": id,
        "workspace_path": workspace_path,
        "plan": read("plan.md"),
        "context": read("context.md"),
        "task_notes": tasks,
    }))
}

/// 写入成员工作状态文件（plan/context——整文件覆盖，调用方为持有
/// 有序锁的成员侧写入；任务笔记走独立文件追加）。
pub fn write(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
    file: &str,
    content: &str,
) -> Result<String> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let name = match file {
        "plan" => "plan.md",
        "context" => "context.md",
        other => bail!("工作状态只支持 plan / context 文件（任务笔记用任务专用入口）: {other}"),
    };
    paths::atomic_write(&dir.join(name), content.as_bytes())?;
    Ok(id)
}

/// 任务笔记：tasks/<note>.md 独立文件写入（互不覆盖）。
pub fn save_task_note(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
    note_name: &str,
    content: &str,
) -> Result<String> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let safe: String = note_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || !c.is_ascii())
        .collect();
    let safe = safe.trim().trim_matches('.').to_string();
    if safe.is_empty() || safe.contains('/') || safe.contains('\\') {
        bail!("任务笔记名不能为空且不得包含路径分隔符");
    }
    let path = dir.join("tasks").join(format!("{safe}.md"));
    if !path.is_absolute() || Path::new(&path).parent() != Some(dir.join("tasks").as_path()) {
        bail!("非法任务笔记名");
    }
    paths::atomic_write(&path, content.as_bytes())?;
    Ok(id)
}

/// 创建成员任务：系统生成稳定编号，文件身份不受名称清洗影响。
pub fn create_task(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
    initial_content: &str,
) -> Result<(String, String)> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let task_id = format!("t-{}", paths::new_id());
    paths::atomic_write(
        &dir.join("tasks").join(format!("{task_id}.md")),
        initial_content.as_bytes(),
    )?;
    Ok((id, task_id))
}

/// 按稳定编号更新任务（整文件覆盖）。
pub fn update_task(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
    task_id: &str,
    content: &str,
) -> Result<String> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let safe = task_id.trim();
    if !safe.starts_with("t-") || safe.contains('/') || safe.contains('\\') {
        bail!("非法任务编号: {task_id}");
    }
    let path = dir.join("tasks").join(format!("{safe}.md"));
    if !path.is_file() {
        bail!("任务不存在: {safe}（先创建再更新）");
    }
    paths::atomic_write(&path, content.as_bytes())?;
    Ok(id)
}

/// 按稳定编号读取任务。
pub fn read_task(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
    task_id: &str,
) -> Result<String> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let safe = task_id.trim();
    if !safe.starts_with("t-") || safe.contains('/') || safe.contains('\\') {
        bail!("非法任务编号: {task_id}");
    }
    let path = dir.join("tasks").join(format!("{safe}.md"));
    if !path.is_file() {
        bail!("任务不存在: {safe}");
    }
    Ok(std::fs::read_to_string(path)?)
}

/// 列举当前 workspace 的任务（编号 + 首行摘要）。
pub fn list_tasks(
    agents: &AgentStore,
    agent_id: &str,
    workspace_path: &str,
) -> Result<serde_json::Value> {
    let id = workspace_id(agents, agent_id, workspace_path)?;
    let dir = state_dir(agents, agent_id, &id)?;
    let mut tasks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("tasks")) {
        let mut files: Vec<_> = entries
            .into_iter()
            .flatten()
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "md")
                    && e.file_name().to_string_lossy().starts_with("t-")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        for entry in files {
            let name = entry.file_name().into_string().unwrap_or_default();
            let task_id = name.trim_end_matches(".md").to_string();
            let summary = std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|body| {
                    body.lines()
                        .find(|line| line.starts_with("# "))
                        .map(|line| line.trim_start_matches("# ").to_string())
                })
                .unwrap_or_default();
            tasks.push(serde_json::json!({ "task_id": task_id, "title": summary }));
        }
    }
    Ok(serde_json::json!({ "workspace_id": id, "tasks": tasks }))
}

/// 列举成员全部工作区的自维护状态（管理页/外部查询视图用）。
pub fn list_all(agents: &AgentStore, agent_id: &str) -> Result<serde_json::Value> {
    let dir = workspaces_dir(agents, agent_id)?;
    let mut items = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let id = entry.file_name().into_string().unwrap_or_default();
        if !id.starts_with("ws-") {
            continue;
        }
        let ws_dir = entry.path();
        let meta = std::fs::read_to_string(ws_dir.join("workspace.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let read = |name: &str| {
            std::fs::read_to_string(ws_dir.join(name))
                .map(|body| body.trim().to_string())
                .unwrap_or_default()
        };
        let notes: Vec<String> = std::fs::read_dir(ws_dir.join("tasks"))
            .map(|entries| {
                let mut names: Vec<String> = entries
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                names.sort();
                names
            })
            .unwrap_or_default();
        items.push(serde_json::json!({
            "workspace_id": id,
            "paths": meta.get("paths").cloned().unwrap_or(serde_json::json!([])),
            "plan": read("plan.md"),
            "context": read("context.md"),
            "task_notes": notes,
            "task_ids": notes.iter().filter(|n| n.starts_with("t-")).collect::<Vec<_>>(),
        }));
    }
    Ok(serde_json::json!({ "workspaces": items }))
}

/// 投递正文注入的工作状态摘要（plan/context 头部，截断保上限）。
pub fn injection_summary(agents: &AgentStore, agent_id: &str, workspace_path: &str) -> String {
    let Ok(state) = load(agents, agent_id, workspace_path) else {
        return String::new();
    };
    let plan = state
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let context = state
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if plan.trim().is_empty() && context.trim().is_empty() {
        return String::new();
    }
    let truncate = |text: &str| {
        if text.chars().count() > STATE_INJECTION_MAX_CHARS / 2 {
            text.chars()
                .take(STATE_INJECTION_MAX_CHARS / 2)
                .collect::<String>()
                + "…"
        } else {
            text.to_string()
        }
    };
    let mut body =
        String::from("\n\n【工作区状态】（你在本工作区的自维护状态，收尾时用工作区状态工具更新）");
    if !plan.trim().is_empty() {
        body.push_str(&format!("\n规划：\n{}", truncate(plan)));
    }
    if !context.trim().is_empty() {
        body.push_str(&format!("\n背景：\n{}", truncate(context)));
    }
    body
}
