//! GUI Memory 管理入口：节点/关系的 CRUD + 测试召回

use crate::core_config::CoreConfigProvider;
use crate::memory::registry::get_or_init_memory_handle_async;

/// GUI Memory 管理：列出全部记忆节点。
pub async fn list_memory_nodes_for_gui(
    config_provider: &CoreConfigProvider,
    query: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> anyhow::Result<Vec<tiangong_memory::MemoryNode>> {
    let status = parse_memory_status(status.as_deref())?;
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    Ok(handle
        .list_nodes(tiangong_memory::MemoryListQuery {
            workspace_id: None,
            query,
            status,
            created_after: None,
            offset: offset.unwrap_or_default(),
            limit: limit.unwrap_or(100),
        })
        .await)
}

/// GUI Memory 管理：统计全部记忆节点真实总数。
pub async fn count_memory_nodes_for_gui(
    config_provider: &CoreConfigProvider,
    query: Option<String>,
    status: Option<String>,
    created_after: Option<String>,
) -> anyhow::Result<usize> {
    let status = parse_memory_status(status.as_deref())?;
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    Ok(handle
        .count_nodes(tiangong_memory::MemoryListQuery {
            workspace_id: None,
            query,
            status,
            created_after,
            offset: 0,
            limit: 0,
        })
        .await)
}

/// GUI Memory 管理：手动新增或调整一条记忆。
pub async fn upsert_manual_memory_for_gui(
    config_provider: &CoreConfigProvider,
    draft: tiangong_memory::ManualMemoryDraft,
) -> anyhow::Result<tiangong_memory::MemoryNode> {
    if draft.title.trim().is_empty() {
        anyhow::bail!("记忆标题不能为空");
    }
    if draft.summary.trim().is_empty() {
        anyhow::bail!("记忆内容不能为空");
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    handle.upsert_manual_memory(draft).await
}

/// GUI Memory 管理：归档或恢复记忆节点。
pub async fn set_memory_node_status_for_gui(
    config_provider: &CoreConfigProvider,
    node_id: String,
    status: String,
) -> anyhow::Result<()> {
    let status = match status.as_str() {
        "active" => tiangong_memory::MemoryStatus::Active,
        "archived" => tiangong_memory::MemoryStatus::Archived,
        other => anyhow::bail!("不支持的记忆状态：{other}"),
    };
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    handle.set_node_status(node_id, status).await
}

/// GUI Memory 管理：列出指定记忆节点的图关系。
pub async fn list_memory_relations_for_gui(
    config_provider: &CoreConfigProvider,
    node_id: String,
) -> anyhow::Result<Vec<tiangong_memory::MemoryRelation>> {
    if node_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    Ok(handle.list_relations(node_id).await)
}

/// GUI Memory 管理：批量列出多个记忆节点的关联关系。
pub async fn list_memory_relations_batch_for_gui(
    config_provider: &CoreConfigProvider,
    node_ids: Vec<String>,
) -> anyhow::Result<Vec<tiangong_memory::MemoryRelation>> {
    let valid_ids = node_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();
    if valid_ids.is_empty() {
        return Ok(Vec::new());
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    Ok(handle.list_relations_batch(valid_ids).await)
}

/// GUI Memory 管理：新增或调整记忆图关系。
pub async fn upsert_memory_relation_for_gui(
    config_provider: &CoreConfigProvider,
    draft: tiangong_memory::MemoryRelationDraft,
) -> anyhow::Result<tiangong_memory::MemoryRelation> {
    if draft.from_node_id.trim().is_empty() || draft.to_node_id.trim().is_empty() {
        anyhow::bail!("关联的起点和终点记忆不能为空");
    }
    if draft.from_node_id == draft.to_node_id {
        anyhow::bail!("记忆不能关联到自身");
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    handle.upsert_relation(draft).await
}

/// GUI Memory 管理：删除记忆图关系。
pub async fn delete_memory_relation_for_gui(
    config_provider: &CoreConfigProvider,
    relation_id: String,
) -> anyhow::Result<()> {
    if relation_id.trim().is_empty() {
        return Ok(());
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    handle.delete_relation(relation_id).await
}

/// GUI Memory 管理：手动测试记忆召回，不写入会话消息链。
pub async fn test_memory_recall_for_gui(
    config_provider: &CoreConfigProvider,
    query: String,
    limit: Option<usize>,
) -> anyhow::Result<Vec<tiangong_memory::RecallHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let handle = get_or_init_memory_handle_async(config_provider)
        .await
        .ok_or_else(|| anyhow::anyhow!("Memory 未启动或初始化失败"))?;
    Ok(handle
        .recall(
            tiangong_memory::RecallAnchors {
                keywords: Vec::new(),
                query,
                strategy: None,
            },
            limit.unwrap_or(8),
        )
        .await)
}

fn parse_memory_status(
    status: Option<&str>,
) -> anyhow::Result<Option<tiangong_memory::MemoryStatus>> {
    match status {
        Some("archived") => Ok(Some(tiangong_memory::MemoryStatus::Archived)),
        Some("active") | None | Some("") => Ok(Some(tiangong_memory::MemoryStatus::Active)),
        Some(other) => anyhow::bail!("不支持的记忆状态：{other}"),
    }
}
