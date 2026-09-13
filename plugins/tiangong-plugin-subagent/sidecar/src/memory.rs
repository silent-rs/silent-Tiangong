//! Agent 长期记忆：`memory/` 下 markdown 文件的管理、注入与整理。
//!
//! 记忆是「经过整理的结论」，不复制全部会话历史：
//! - 手动：管理页/工具读写指定文件；
//! - 自动归档：Run 完成后把「任务目标 + 结果」追加到 tasks.md；
//! - 从关联会话整理：提取每轮「用户消息 + 最终回复（截断）」生成 session-notes.md。

use anyhow::{Context, Result, bail};

use tiangong_plugin_subagent_protocol::ops::MemoryFileEntry;

use crate::agent_store::AgentStore;
use crate::paths::{now_string, validate_id_segment};

/// 追加到 tasks.md 的单条任务结论最大长度。
const TASK_ENTRY_MAX_CHARS: usize = 800;
/// 运行注入（begin 帧/投递消息）携带的记忆摘要上限。
const INJECTION_MAX_CHARS: usize = 6000;
/// 记忆文件名单段约束。
const MEMORY_NAME_MAX: usize = 96;

fn memory_dir(agents: &AgentStore, agent_id: &str) -> Result<std::path::PathBuf> {
    validate_id_segment(agent_id)?;
    let dir = agents.root().join(agent_id).join("memory");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建记忆目录失败: {}", dir.display()))?;
    Ok(dir)
}

/// 校验记忆文件名（markdown、单段、无逃逸）。
fn validate_memory_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MEMORY_NAME_MAX
        && name.ends_with(".md")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && !name.contains("..");
    if valid {
        Ok(())
    } else {
        bail!("记忆文件名必须是简单 markdown 文件名（如 notes.md）: {name:?}")
    }
}

/// 记忆文件列表。
pub fn list(agents: &AgentStore, agent_id: &str) -> Result<Vec<MemoryFileEntry>> {
    let dir = memory_dir(agents, agent_id)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => bail!("读取记忆目录失败: {error}"),
    };
    let mut output = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let updated_at = metadata.modified().ok().map(|time| {
            let datetime: chrono::DateTime<chrono::Local> = time.into();
            datetime.naive_local().to_string()
        });
        output.push(MemoryFileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            size_bytes: metadata.len(),
            updated_at,
        });
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(output)
}

/// 读取记忆文件。
pub fn read(agents: &AgentStore, agent_id: &str, name: &str) -> Result<String> {
    validate_memory_name(name)?;
    let path = memory_dir(agents, agent_id)?.join(name);
    std::fs::read_to_string(&path).with_context(|| format!("记忆文件不存在: {name}"))
}

/// 写入（创建或覆盖）记忆文件。
pub fn write(agents: &AgentStore, agent_id: &str, name: &str, content: &str) -> Result<()> {
    validate_memory_name(name)?;
    let path = memory_dir(agents, agent_id)?.join(name);
    crate::paths::atomic_write(&path, content.as_bytes())
}

/// 删除记忆文件。
pub fn delete(agents: &AgentStore, agent_id: &str, name: &str) -> Result<()> {
    validate_memory_name(name)?;
    let path = memory_dir(agents, agent_id)?.join(name);
    std::fs::remove_file(&path).with_context(|| format!("删除记忆文件失败: {name}"))
}

/// 追加记忆条目（AI 工具 append_agent_memory）：默认 notes.md，
/// 可指定目标文件（如成长经验写 lessons.md）。
pub fn append_note(
    agents: &AgentStore,
    agent_id: &str,
    content: &str,
    note: Option<&str>,
    memory_name: Option<&str>,
) -> Result<String> {
    if content.trim().is_empty() {
        bail!("记忆内容不能为空");
    }
    let name = memory_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("notes.md");
    validate_memory_name(name)?;
    let timestamp = now_string();
    let entry = format!(
        "\n## {}{}\n\n{}\n",
        timestamp,
        note.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("（{s}）"))
            .unwrap_or_default(),
        content.trim()
    );
    let path = memory_dir(agents, agent_id)?.join(name);
    let mut body = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        format!(
            "# {} 的长期记忆\n",
            agents.load(agent_id).map(|c| c.name).unwrap_or_default()
        )
    });
    body.push_str(&entry);
    crate::paths::atomic_write(&path, body.as_bytes())?;
    Ok(name.to_string())
}

/// Run 完成后的结论归档（append 到 tasks.md）。
pub fn archive_run_result(agents: &AgentStore, agent_id: &str, goal: &str, result: &str) {
    let result = truncate_chars(result.trim(), TASK_ENTRY_MAX_CHARS);
    if goal.trim().is_empty() && result.is_empty() {
        return;
    }
    let entry = format!(
        "\n## {} — {}\n\n{}\n",
        now_string(),
        truncate_chars(goal.trim(), 200),
        if result.is_empty() {
            "（无结果摘要）"
        } else {
            &result
        }
    );
    let path = match memory_dir(agents, agent_id) {
        Ok(dir) => dir.join("tasks.md"),
        Err(error) => {
            tracing::warn!(%error, "定位记忆目录失败，跳过归档");
            return;
        }
    };
    let mut body = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        format!(
            "# {} 的任务结论归档\n\n每次运行完成后自动追加「目标 + 结果」结论。\n",
            agents.load(agent_id).map(|c| c.name).unwrap_or_default()
        )
    });
    body.push_str(&entry);
    if let Err(error) = crate::paths::atomic_write(&path, body.as_bytes()) {
        tracing::warn!(%error, "归档任务结论失败");
    }
}

/// 经验文件（成长沉淀目标）：注入时优先并给予最大份额。
const LESSONS_FILE: &str = "lessons.md";
/// 经验文件注入配额。
const LESSONS_QUOTA_CHARS: usize = 2400;
/// 笔记文件注入配额。
const NOTES_QUOTA_CHARS: usize = 1200;

/// 记忆注入快照：按价值分文件配额——经验（lessons）优先且份额最大，
/// 其次 notes，任务流水与其余文件填充剩余额度，总量上限不变。
pub fn injection_snapshot(agents: &AgentStore, agent_id: &str) -> String {
    let Ok(entries) = list(agents, agent_id) else {
        return String::new();
    };
    // 分层排序：经验优先，其次笔记，最后其余文件（组内保持原序）。
    let mut prioritized = entries;
    prioritized.sort_by_key(|entry| match entry.name.as_str() {
        LESSONS_FILE => 0,
        "notes.md" => 1,
        _ => 2,
    });
    let mut parts = Vec::new();
    let mut total = 0usize;
    for entry in prioritized {
        let Ok(content) = read(agents, agent_id, &entry.name) else {
            continue;
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        let quota = match entry.name.as_str() {
            LESSONS_FILE => LESSONS_QUOTA_CHARS,
            "notes.md" => NOTES_QUOTA_CHARS,
            _ => INJECTION_MAX_CHARS,
        };
        let part = format!(
            "### {}（memory/{}）\n{}\n",
            entry.name,
            entry.name,
            truncate_chars(content, quota)
        );
        total += part.chars().count();
        parts.push(part);
        if total >= INJECTION_MAX_CHARS {
            break;
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    let mut snapshot = parts.join("\n");
    if snapshot.chars().count() > INJECTION_MAX_CHARS {
        snapshot = truncate_chars(&snapshot, INJECTION_MAX_CHARS);
    }
    snapshot
}

/// 按字符数截断（避免切开多字节字符）。
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let truncated: String = value.chars().take(max).collect();
    format!("{truncated}…（已截断）")
}
