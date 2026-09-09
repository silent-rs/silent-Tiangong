//! 成员工作区状态：agents/<id>/workspaces/<workspace-id>/ 下的成员自维护
//! 工作状态（plan/context/tasks）。sidecar 只提供安全存取与稳定身份
//! 解析（路径→workspace-id），不理解业务内容——工作含义由成员维护，
//! 执行事实由运行记录维护，两类状态分离。

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::agent_store::AgentStore;
use crate::paths;

/// 工作区状态注入摘要上限（成员规划过长时截断，避免挤占任务正文）。
const STATE_INJECTION_MAX_CHARS: usize = 1500;

/// 解析（或首次登记）workspace 稳定身份：agents/<id>/workspaces/index.json
/// 维护「本机路径 → workspace-id」映射；路径只是位置，id 才是身份。
///
/// 登记全程（查询、创建、写回）持文件锁：宿主 sidecar 与外部 MCP 进程
/// 可能同时访问同一 agents 目录，单次原子写不能保护读-改-写整体。
fn workspace_id(agents: &AgentStore, agent_id: &str, workspace_path: &str) -> Result<String> {
    let dir = workspaces_dir(agents, agent_id)?;
    let _guard = IndexLock::acquire(&dir)?;
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
    std::fs::create_dir_all(&ws_dir)?;
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

/// 工作区索引的跨进程排他锁（目录下 `.index.lock` 文件，flock 独占）。
///
/// 保护 workspace 身份登记的读-改-写整体：宿主 sidecar 与外部 MCP 进程
/// 可能同时登记同一工作区，无锁时并发读到旧索引会各自分配新身份并
/// 相互覆盖。RAII：Drop 解锁，锁文件本身不删除。
struct IndexLock {
    _file: std::fs::File,
}

impl IndexLock {
    fn acquire(dir: &std::path::Path) -> Result<Self> {
        use fs2::FileExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".index.lock"))?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self._file);
    }
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

/// 读取成员在某工作区的自维护状态（plan + context + task）。
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
    Ok(serde_json::json!({
        "workspace_id": id,
        "workspace_path": workspace_path,
        "plan": read("plan.md"),
        "context": read("context.md"),
        "task": read("task.md"),
    }))
}

/// 写入成员工作状态文件（plan/context——整文件覆盖，调用方为持有
/// 有序锁的成员侧写入；task 为当前工作与待办，三文件均整文件覆盖）。
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
        "task" => "task.md",
        other => bail!("工作状态只支持 plan / context / task 文件: {other}"),
    };
    paths::atomic_write(&dir.join(name), content.as_bytes())?;
    Ok(id)
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
        items.push(serde_json::json!({
            "workspace_id": id,
            "paths": meta.get("paths").cloned().unwrap_or(serde_json::json!([])),
            "plan": read("plan.md"),
            "context": read("context.md"),
            "task": read("task.md"),
        }));
    }
    Ok(serde_json::json!({ "workspaces": items }))
}

/// 投递正文注入的工作状态摘要（plan/context 头部，截断保上限）。
pub fn injection_summary(agents: &AgentStore, agent_id: &str, workspace_path: &str) -> String {
    let Ok(state) = load(agents, agent_id, workspace_path) else {
        return String::new();
    };
    let task = state
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let plan = state
        .get("plan")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let context = state
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if task.trim().is_empty() && plan.trim().is_empty() && context.trim().is_empty() {
        return String::new();
    }
    let per_part = STATE_INJECTION_MAX_CHARS / 3;
    let truncate = |text: &str| {
        if text.chars().count() > per_part {
            text.chars().take(per_part).collect::<String>() + "…"
        } else {
            text.to_string()
        }
    };
    let mut body =
        String::from("\n\n【工作区状态】（你在本工作区的自维护状态，收尾时用工作区状态工具更新）");
    if !task.trim().is_empty() {
        body.push_str(&format!("\n当前工作：\n{}", truncate(task)));
    }
    if !plan.trim().is_empty() {
        body.push_str(&format!("\n规划：\n{}", truncate(plan)));
    }
    if !context.trim().is_empty() {
        body.push_str(&format!("\n背景：\n{}", truncate(context)));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立存储根下构造带一个成员的 AgentStore（serial：TIANGONG_STORAGE_ROOT
    /// 是进程级环境变量，测试间须串行防互染）。
    fn store_with_agent(tag: &str) -> (AgentStore, tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        // serial 测试内单线程设置（edition 2024 set_var 为 unsafe）。
        unsafe { std::env::set_var("TIANGONG_STORAGE_ROOT", root.path()) };
        let store = AgentStore::open().unwrap();
        let config = store
            .create(
                &format!("测试成员-{tag}"),
                "并发登记测试",
                tiangong_plugin_subagent_protocol::config::BackendKind::TiangongSession,
                None,
                Some("sess-test"),
                tiangong_plugin_subagent_protocol::config::WorkspacePolicy::ReadOnly,
                None,
            )
            .unwrap();
        (store, root, config.id)
    }

    #[serial_test::serial]
    #[test]
    fn 并发登记同一工作区只有一个稳定身份() {
        let (store, _root, agent_id) = store_with_agent("concurrent");
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().to_string_lossy().into_owned();

        // 8 线程同时首次登记同一工作区：文件锁保证读-改-写整体串行，
        // 全部拿到同一 id（此前无锁时会分裂出多个身份并相互覆盖索引）。
        let mut handles = Vec::new();
        for _ in 0..8 {
            let agent = agent_id.clone();
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                // 每线程独立 open（同一存储根），模拟宿主与外部 MCP 双进程并发登记。
                let store = AgentStore::open().unwrap();
                workspace_id(&store, &agent, &path).unwrap()
            }));
        }
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = &ids[0];
        assert!(ids.iter().all(|id| id == first), "身份分裂: {ids:?}");

        // 索引只登记一条。
        let index =
            std::fs::read_to_string(store.root().join(&agent_id).join("workspaces/index.json"))
                .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&index).unwrap();
        assert_eq!(parsed["workspaces"].as_array().unwrap().len(), 1);
    }

    #[serial_test::serial]
    #[test]
    fn 同一工作区重复登记返回同一身份() {
        let (store, _root, agent_id) = store_with_agent("stable");
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().to_string_lossy().into_owned();
        let first = workspace_id(&store, &agent_id, &path).unwrap();
        let second = workspace_id(&store, &agent_id, &path).unwrap();
        assert_eq!(first, second);
    }
}
