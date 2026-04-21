//! SQLite 加密连接与 CRUD 操作

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::types::{
    Decision, Entity, EntityType, Episode, ExpandedMemory, MemoryKind, MemoryScopeType,
    MemoryStatus, VectorPoint,
};

use super::schema;

/// Memory 元数据库（加密 SQLite）
pub(crate) struct MemoryDb {
    conn: Connection,
}

impl MemoryDb {
    /// 打开或创建加密数据库，并初始化 Schema
    pub(crate) fn open() -> Result<Self> {
        let db_path = db_file_path();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建数据库目录失败: {}", parent.display()))?;
        }

        let conn = open_encrypted_conn(&db_path)?;
        schema::init_schema(&conn)?;

        Ok(Self { conn })
    }

    /// 插入 Episode 到 memory_nodes 和 episodes 表
    pub(crate) fn insert_episode(
        &self,
        episode: &Episode,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Local::now().naive_local().to_string();
        let keywords = serde_json::to_string(&episode.keywords)?;
        let tool_calls = serde_json::to_string(&episode.tool_calls)?;
        let outcome = serde_json::to_string(&episode.outcome)?;
        let full_content = serde_json::to_string(episode)?;

        // 写入 memory_nodes（scope_id 来自 workspace_id，不再硬编码 NULL）
        self.conn
            .execute(
                "INSERT OR REPLACE INTO memory_nodes
                 (id, kind, scope_type, scope_id, title, summary, keywords, importance,
                  confidence, status, source, usage_count, created_at, updated_at)
                 VALUES (?1, 'episode', 'workspace', ?2, ?3, ?4, ?5, ?6, 1.0, 'active', ?7, 0, ?8, ?8)",
                rusqlite::params![
                    episode.id,
                    workspace_id,
                    episode.title,
                    episode.summary,
                    keywords,
                    episode.importance,
                    episode.session_id,
                    now,
                ],
            )
            .with_context(|| "写入 memory_nodes 失败")?;

        // 写入 episodes
        self.conn
            .execute(
                "INSERT OR REPLACE INTO episodes (id, session_id, outcome, tool_calls, full_content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    episode.id,
                    episode.session_id,
                    outcome,
                    tool_calls,
                    full_content,
                ],
            )
            .with_context(|| "写入 episodes 失败")?;

        Ok(())
    }

    /// 插入或更新 Entity 到 memory_nodes 和 entities 表
    #[allow(dead_code)]
    pub(crate) fn upsert_entity(&self, entity: &Entity, workspace_id: Option<&str>) -> Result<()> {
        let keywords = serde_json::to_string(&entity.related_episodes)?;
        let related_episodes = serde_json::to_string(&entity.related_episodes)?;
        let full_content = serde_json::to_string(entity)?;

        self.upsert_memory_node(
            &entity.id,
            MemoryKind::Entity,
            MemoryScopeType::Workspace,
            workspace_id,
            &entity.name,
            &entity.description,
            &keywords,
            entity.importance,
            entity.file_path.as_deref(),
            &entity.created_at,
            &entity.updated_at,
        )?;

        self.conn
            .execute(
                "INSERT OR REPLACE INTO entities
                 (id, entity_type, file_path, related_episodes, full_content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    entity.id,
                    entity_type_to_str(&entity.entity_type),
                    entity.file_path,
                    related_episodes,
                    full_content,
                ],
            )
            .with_context(|| "写入 entities 失败")?;

        Ok(())
    }

    /// 根据 id 查询 Entity
    #[allow(dead_code)]
    pub(crate) fn get_entity(&self, entity_id: &str) -> Result<Option<Entity>> {
        let mut stmt = self
            .conn
            .prepare("SELECT full_content FROM entities WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![entity_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let full_content: String = row.get(0)?;
        let entity = serde_json::from_str(&full_content).with_context(|| "解析 Entity 失败")?;
        Ok(Some(entity))
    }

    /// 列出工作区下的 Entity
    #[allow(dead_code)]
    pub(crate) fn list_entities(&self, workspace_id: Option<&str>) -> Result<Vec<Entity>> {
        let mut entities = Vec::new();
        if let Some(workspace_id) = workspace_id {
            let mut stmt = self.conn.prepare(
                "SELECT e.full_content
                 FROM entities e
                 JOIN memory_nodes n ON n.id = e.id
                 WHERE n.scope_type = 'workspace' AND n.scope_id = ?1
                 ORDER BY n.updated_at DESC",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let full_content = row?;
                let entity = serde_json::from_str(&full_content)
                    .with_context(|| "解析 Entity 列表项失败")?;
                entities.push(entity);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT e.full_content
                 FROM entities e
                 JOIN memory_nodes n ON n.id = e.id
                 WHERE n.scope_type = 'workspace' AND n.scope_id IS NULL
                 ORDER BY n.updated_at DESC",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let full_content = row?;
                let entity = serde_json::from_str(&full_content)
                    .with_context(|| "解析 Entity 列表项失败")?;
                entities.push(entity);
            }
        }
        Ok(entities)
    }

    /// 删除 Entity
    #[allow(dead_code)]
    pub(crate) fn delete_entity(&self, entity_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM entities WHERE id = ?1",
                rusqlite::params![entity_id],
            )
            .with_context(|| "删除 entities 记录失败")?;
        self.conn
            .execute(
                "DELETE FROM memory_nodes WHERE id = ?1",
                rusqlite::params![entity_id],
            )
            .with_context(|| "删除 entity 对应 memory_nodes 记录失败")?;
        Ok(())
    }

    /// 插入或更新 Decision 到 memory_nodes 和 decisions 表
    #[allow(dead_code)]
    pub(crate) fn upsert_decision(
        &self,
        decision: &Decision,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let keywords = serde_json::to_string(&decision.reasons)?;
        let alternatives = serde_json::to_string(&decision.alternatives)?;
        let reasons = serde_json::to_string(&decision.reasons)?;
        let episode_ids = serde_json::to_string(&decision.episode_ids)?;
        let full_content = serde_json::to_string(decision)?;

        self.upsert_memory_node(
            &decision.id,
            MemoryKind::Decision,
            MemoryScopeType::Workspace,
            workspace_id,
            &decision.title,
            &decision.context,
            &keywords,
            0.7,
            Some(&decision.chosen),
            &decision.created_at,
            &decision.created_at,
        )?;

        self.conn
            .execute(
                "INSERT OR REPLACE INTO decisions
                 (id, context, alternatives, chosen, reasons, episode_ids, full_content)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    decision.id,
                    decision.context,
                    alternatives,
                    decision.chosen,
                    reasons,
                    episode_ids,
                    full_content,
                ],
            )
            .with_context(|| "写入 decisions 失败")?;

        Ok(())
    }

    /// 根据 id 查询 Decision
    #[allow(dead_code)]
    pub(crate) fn get_decision(&self, decision_id: &str) -> Result<Option<Decision>> {
        let mut stmt = self
            .conn
            .prepare("SELECT full_content FROM decisions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![decision_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let full_content: String = row.get(0)?;
        let decision = serde_json::from_str(&full_content).with_context(|| "解析 Decision 失败")?;
        Ok(Some(decision))
    }

    /// 列出工作区下的 Decision
    #[allow(dead_code)]
    pub(crate) fn list_decisions(&self, workspace_id: Option<&str>) -> Result<Vec<Decision>> {
        let mut decisions = Vec::new();
        if let Some(workspace_id) = workspace_id {
            let mut stmt = self.conn.prepare(
                "SELECT d.full_content
                 FROM decisions d
                 JOIN memory_nodes n ON n.id = d.id
                 WHERE n.scope_type = 'workspace' AND n.scope_id = ?1
                 ORDER BY n.updated_at DESC",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let full_content = row?;
                let decision = serde_json::from_str(&full_content)
                    .with_context(|| "解析 Decision 列表项失败")?;
                decisions.push(decision);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT d.full_content
                 FROM decisions d
                 JOIN memory_nodes n ON n.id = d.id
                 WHERE n.scope_type = 'workspace' AND n.scope_id IS NULL
                 ORDER BY n.updated_at DESC",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let full_content = row?;
                let decision = serde_json::from_str(&full_content)
                    .with_context(|| "解析 Decision 列表项失败")?;
                decisions.push(decision);
            }
        }
        Ok(decisions)
    }

    /// 删除 Decision
    #[allow(dead_code)]
    pub(crate) fn delete_decision(&self, decision_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM decisions WHERE id = ?1",
                rusqlite::params![decision_id],
            )
            .with_context(|| "删除 decisions 记录失败")?;
        self.conn
            .execute(
                "DELETE FROM memory_nodes WHERE id = ?1",
                rusqlite::params![decision_id],
            )
            .with_context(|| "删除 decision 对应 memory_nodes 记录失败")?;
        Ok(())
    }

    /// 查询最近的 Episode 摘要（用于 MesoRumination）
    #[allow(dead_code)]
    pub(crate) fn recent_episode_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.title, n.keywords
             FROM memory_nodes n
             WHERE n.kind = 'episode' AND n.status = 'active'
             ORDER BY n.created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap_or_default(),
                    row.get::<_, String>(1).unwrap_or_default(),
                ))
            })?
            .filter_map(|r| r.ok())
            .map(|(title, kw_json)| {
                let kws: Vec<String> = serde_json::from_str(&kw_json).unwrap_or_default();
                (title, kws)
            })
            .collect();
        Ok(rows)
    }

    /// 查询最近的完整 Episode，供 MesoRumination 提炼 Entity / Decision。
    pub(crate) fn recent_episodes(
        &self,
        workspace_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) =
            if let Some(workspace_id) = workspace_id {
                (
                    "SELECT ep.full_content
                     FROM episodes ep
                     JOIN memory_nodes n ON n.id = ep.id
                     WHERE n.kind = 'episode'
                       AND n.status = 'active'
                       AND n.scope_type = 'workspace'
                       AND n.scope_id = ?1
                     ORDER BY n.created_at DESC
                     LIMIT ?2",
                    vec![Box::new(workspace_id.to_string()), Box::new(limit as i64)],
                )
            } else {
                (
                    "SELECT ep.full_content
                     FROM episodes ep
                     JOIN memory_nodes n ON n.id = ep.id
                     WHERE n.kind = 'episode'
                       AND n.status = 'active'
                       AND n.scope_type = 'workspace'
                       AND n.scope_id IS NULL
                     ORDER BY n.created_at DESC
                     LIMIT ?1",
                    vec![Box::new(limit as i64)],
                )
            };

        let mut stmt = self.conn.prepare(sql)?;
        let params = params.iter().map(|item| item.as_ref()).collect::<Vec<_>>();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            row.get::<_, String>(0)
        })?;
        let mut episodes = Vec::new();
        for row in rows {
            let full_content = row?;
            let episode = serde_json::from_str(&full_content)
                .with_context(|| "解析 Meso Episode 列表项失败")?;
            episodes.push(episode);
        }
        Ok(episodes)
    }

    /// 列出超过指定天数未使用且重要度低于阈值的节点（用于归档）
    pub(crate) fn list_stale_nodes(
        &self,
        days_threshold: i64,
        importance_threshold: f64,
    ) -> Result<Vec<(String, f64)>> {
        let cutoff = chrono::Local::now().naive_local() - chrono::TimeDelta::days(days_threshold);
        let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S%.f").to_string();

        let mut stmt = self.conn.prepare(
            "SELECT id, importance FROM memory_nodes
             WHERE status = 'active'
               AND (last_used_at IS NULL OR last_used_at < ?1)
               AND importance < ?2
             ORDER BY importance ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![cutoff_str, importance_threshold], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// 更新节点状态（active → archived）
    pub(crate) fn update_node_status(&self, node_id: &str, status: &MemoryStatus) -> Result<()> {
        let status_str = match status {
            MemoryStatus::Active => "active",
            MemoryStatus::Archived => "archived",
        };
        let now = chrono::Local::now().naive_local().to_string();
        self.conn.execute(
            "UPDATE memory_nodes SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status_str, now, node_id],
        )?;
        Ok(())
    }

    /// 写入或更新内置向量索引点。
    pub(crate) fn upsert_vector(&self, point: &VectorPoint) -> Result<()> {
        let vector_json = serde_json::to_string(&point.vector)?;
        let now = chrono::Local::now().naive_local().to_string();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO memory_vectors
                 (node_id, title, summary, kind, importance, dimension, vector, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    point.node_id,
                    point.title,
                    point.summary,
                    memory_kind_to_str(point.kind.clone()),
                    point.importance,
                    point.vector.len() as i64,
                    vector_json,
                    now,
                ],
            )
            .with_context(|| "写入 memory_vectors 失败")?;
        Ok(())
    }

    /// 加载指定维度的全部向量点，供内置 flat search 使用。
    pub(crate) fn list_vectors(&self, dimension: usize) -> Result<Vec<VectorPoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, title, summary, kind, importance, vector
             FROM memory_vectors
             WHERE dimension = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![dimension as i64], |row| {
            let kind: String = row.get(3)?;
            let vector_json: String = row.get(5)?;
            let vector: Vec<f32> = serde_json::from_str(&vector_json).unwrap_or_default();
            Ok(VectorPoint {
                node_id: row.get(0)?,
                title: row.get(1)?,
                summary: row.get(2)?,
                kind: str_to_memory_kind(&kind),
                importance: row.get(4)?,
                vector,
            })
        })?;

        let mut points = Vec::new();
        for row in rows {
            points.push(row.with_context(|| "读取 memory_vectors 行失败")?);
        }
        Ok(points)
    }

    /// 按节点 ID 加载完整内容，供 LoadDepth2 定向展开使用。
    ///
    /// 返回顺序与传入 ID 顺序一致；不存在或已归档的节点会被跳过。
    pub(crate) fn load_expanded_memories(
        &self,
        node_ids: &[String],
    ) -> Result<Vec<ExpandedMemory>> {
        let mut items = Vec::new();
        for node_id in node_ids {
            let item = self.load_expanded_memory(node_id)?;
            if let Some(item) = item {
                self.mark_node_used(node_id)?;
                items.push(item);
            }
        }
        Ok(items)
    }

    /// 删除内置向量索引点。
    #[allow(dead_code)]
    pub(crate) fn delete_vector(&self, node_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM memory_vectors WHERE node_id = ?1",
                rusqlite::params![node_id],
            )
            .with_context(|| "删除 memory_vectors 记录失败")?;
        Ok(())
    }

    fn load_expanded_memory(&self, node_id: &str) -> Result<Option<ExpandedMemory>> {
        let row = self
            .conn
            .query_row(
                "SELECT n.kind, n.title, n.summary,
                        ep.full_content,
                        en.full_content,
                        de.full_content,
                        ev.evidence_path,
                        ev.byte_size
                 FROM memory_nodes n
                 LEFT JOIN episodes ep ON ep.id = n.id
                 LEFT JOIN entities en ON en.id = n.id
                 LEFT JOIN decisions de ON de.id = n.id
                 LEFT JOIN evidence ev ON ev.id = n.id
                 WHERE n.id = ?1 AND n.status = 'active'",
                rusqlite::params![node_id],
                |row| {
                    Ok(ExpandedRow {
                        kind: row.get(0)?,
                        title: row.get(1)?,
                        summary: row.get(2)?,
                        episode_content: row.get(3)?,
                        entity_content: row.get(4)?,
                        decision_content: row.get(5)?,
                        evidence_path: row.get(6)?,
                        evidence_byte_size: row.get(7)?,
                    })
                },
            )
            .optional()
            .with_context(|| format!("查询展开节点失败: {node_id}"))?;

        let Some(row) = row else {
            return Ok(None);
        };
        let full_content = match row.kind.as_str() {
            "episode" => row.episode_content,
            "entity" => row.entity_content,
            "decision" => row.decision_content,
            "evidence" => row.evidence_path.map(|path| {
                serde_json::json!({
                    "kind": "evidence",
                    "title": row.title,
                    "summary": row.summary,
                    "evidence_path": path,
                    "byte_size": row.evidence_byte_size.unwrap_or_default(),
                })
                .to_string()
            }),
            _ => None,
        };

        Ok(full_content.map(|full_content| ExpandedMemory {
            node_id: node_id.to_string(),
            full_content,
        }))
    }

    fn mark_node_used(&self, node_id: &str) -> Result<()> {
        let now = chrono::Local::now().naive_local().to_string();
        self.conn
            .execute(
                "UPDATE memory_nodes
                 SET usage_count = usage_count + 1, last_used_at = ?1, updated_at = ?1
                 WHERE id = ?2",
                rusqlite::params![now, node_id],
            )
            .with_context(|| format!("更新节点使用状态失败: {node_id}"))?;
        Ok(())
    }

    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    fn upsert_memory_node(
        &self,
        id: &str,
        kind: MemoryKind,
        scope_type: MemoryScopeType,
        scope_id: Option<&str>,
        title: &str,
        summary: &str,
        keywords: &str,
        importance: f32,
        source: Option<&str>,
        created_at: &str,
        updated_at: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO memory_nodes
                 (id, kind, scope_type, scope_id, title, summary, keywords, importance,
                  confidence, status, source, usage_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1.0, 'active', ?9, 0, ?10, ?11)",
                rusqlite::params![
                    id,
                    memory_kind_to_str(kind),
                    scope_type_to_str(scope_type),
                    scope_id,
                    title,
                    summary,
                    keywords,
                    importance,
                    source,
                    created_at,
                    updated_at,
                ],
            )
            .with_context(|| "写入 memory_nodes 失败")?;
        Ok(())
    }
}

struct ExpandedRow {
    kind: String,
    title: String,
    summary: String,
    episode_content: Option<String>,
    entity_content: Option<String>,
    decision_content: Option<String>,
    evidence_path: Option<String>,
    evidence_byte_size: Option<i64>,
}

/// 获取数据库文件路径
fn db_file_path() -> PathBuf {
    memory_base_path().join("metadata.db")
}

fn memory_base_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
}

fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

/// 生成 SQLite 加密密码（基于 home 目录绝对路径的 SHA-256 hash）
fn derive_db_password() -> String {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    let abs_path = home.canonicalize().unwrap_or(home);
    let digest = Sha256::digest(abs_path.to_string_lossy().as_bytes());
    hex::encode(digest)
}

#[allow(dead_code)]
fn memory_kind_to_str(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Episode => "episode",
        MemoryKind::Entity => "entity",
        MemoryKind::Decision => "decision",
        MemoryKind::Evidence => "evidence",
    }
}

fn str_to_memory_kind(kind: &str) -> MemoryKind {
    match kind {
        "entity" => MemoryKind::Entity,
        "decision" => MemoryKind::Decision,
        "evidence" => MemoryKind::Evidence,
        _ => MemoryKind::Episode,
    }
}

#[allow(dead_code)]
fn scope_type_to_str(scope_type: MemoryScopeType) -> &'static str {
    match scope_type {
        MemoryScopeType::Global => "global",
        MemoryScopeType::Workspace => "workspace",
        MemoryScopeType::Session => "session",
    }
}

#[allow(dead_code)]
fn entity_type_to_str(entity_type: &EntityType) -> &'static str {
    match entity_type {
        EntityType::Project => "project",
        EntityType::Repository => "repository",
        EntityType::Server => "server",
        EntityType::Skill => "skill",
        EntityType::Provider => "provider",
        EntityType::Document => "document",
        EntityType::Module => "module",
    }
}

/// 打开加密数据库连接
fn open_encrypted_conn(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("打开数据库失败: {}", db_path.display()))?;

    let password = derive_db_password();
    conn.pragma_update(None, "key", &password)
        .with_context(|| "设置数据库加密密钥失败")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .with_context(|| "设置 WAL 模式失败")?;

    Ok(conn)
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;

    /// 创建仅用于测试的内存数据库（不加密）
    pub(crate) fn open_in_memory() -> Result<MemoryDb> {
        let conn = Connection::open_in_memory().with_context(|| "创建测试内存数据库失败")?;
        schema::init_schema(&conn)?;
        Ok(MemoryDb { conn })
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::open_in_memory;
    use crate::types::{Decision, Entity, EntityType, Episode, EpisodeOutcome};

    fn make_episode(session_id: &str) -> Episode {
        Episode::new(
            session_id.to_string(),
            "测试标题".to_string(),
            "测试摘要".to_string(),
            EpisodeOutcome::Success,
            vec!["关键词A".to_string(), "关键词B".to_string()],
            vec!["tool_call_1".to_string()],
            0.7,
        )
    }

    fn make_entity(id: &str) -> Entity {
        let now = chrono::Local::now().naive_local().to_string();
        Entity {
            id: id.to_string(),
            name: "memory-system".to_string(),
            entity_type: EntityType::Project,
            description: "memory system project".to_string(),
            file_path: Some("/tmp/memory-system".to_string()),
            related_episodes: vec!["ep-1".to_string(), "ep-2".to_string()],
            importance: 0.8,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    fn make_decision(id: &str) -> Decision {
        Decision {
            id: id.to_string(),
            title: "use tcp ipc".to_string(),
            context: "windows does not support unix socket path".to_string(),
            alternatives: vec!["uds".to_string(), "named pipe".to_string()],
            chosen: "tcp loopback".to_string(),
            reasons: vec!["cross platform".to_string(), "easy to test".to_string()],
            episode_ids: vec!["ep-10".to_string()],
            created_at: chrono::Local::now().naive_local().to_string(),
        }
    }

    #[test]
    fn insert_episode_stores_workspace_id_in_scope_id() {
        let db = open_in_memory().unwrap();
        let episode = make_episode("sess-001");
        let workspace_id = "ws-project-x";

        db.insert_episode(&episode, Some(workspace_id)).unwrap();

        // 验证 scope_id 正确写入
        let stored: Option<String> = db
            .conn
            .query_row(
                "SELECT scope_id FROM memory_nodes WHERE id = ?1",
                rusqlite::params![episode.id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(stored.as_deref(), Some(workspace_id));
    }

    #[test]
    fn insert_episode_scope_id_is_null_when_no_workspace() {
        let db = open_in_memory().unwrap();
        let episode = make_episode("sess-002");

        db.insert_episode(&episode, None).unwrap();

        let stored: Option<String> = db
            .conn
            .query_row(
                "SELECT scope_id FROM memory_nodes WHERE id = ?1",
                rusqlite::params![episode.id],
                |row| row.get(0),
            )
            .unwrap();

        assert!(stored.is_none(), "无 workspace_id 时 scope_id 应为 NULL");
    }

    #[test]
    fn recent_episode_summaries_returns_correct_count() {
        let db = open_in_memory().unwrap();
        for i in 0..5 {
            let ep = make_episode(&format!("sess-{i}"));
            db.insert_episode(&ep, Some("ws-test")).unwrap();
        }

        let summaries = db.recent_episode_summaries(3).unwrap();
        assert_eq!(summaries.len(), 3, "应返回最近 3 条");
    }

    #[test]
    fn entity_crud_roundtrip_works() {
        let db = open_in_memory().unwrap();
        let entity = make_entity("entity-1");

        db.upsert_entity(&entity, Some("ws-entity")).unwrap();

        let loaded = db.get_entity("entity-1").unwrap().expect("entity 应存在");
        assert_eq!(loaded.name, entity.name);

        let listed = db.list_entities(Some("ws-entity")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, entity.id);

        db.delete_entity("entity-1").unwrap();
        assert!(db.get_entity("entity-1").unwrap().is_none());
    }

    #[test]
    fn decision_crud_roundtrip_works() {
        let db = open_in_memory().unwrap();
        let decision = make_decision("decision-1");

        db.upsert_decision(&decision, Some("ws-decision")).unwrap();

        let loaded = db
            .get_decision("decision-1")
            .unwrap()
            .expect("decision 应存在");
        assert_eq!(loaded.title, decision.title);

        let listed = db.list_decisions(Some("ws-decision")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, decision.id);

        db.delete_decision("decision-1").unwrap();
        assert!(db.get_decision("decision-1").unwrap().is_none());
    }

    #[test]
    fn load_expanded_memories_returns_full_content_in_requested_order() {
        let db = open_in_memory().unwrap();
        let episode_a = make_episode("sess-depth2-a");
        let episode_b = make_episode("sess-depth2-b");
        let id_a = episode_a.id.clone();
        let id_b = episode_b.id.clone();

        db.insert_episode(&episode_a, Some("ws-depth2")).unwrap();
        db.insert_episode(&episode_b, Some("ws-depth2")).unwrap();

        let expanded = db
            .load_expanded_memories(&[id_b.clone(), "missing-node".to_string(), id_a.clone()])
            .unwrap();

        assert_eq!(expanded.len(), 2);
        assert_eq!(expanded[0].node_id, id_b);
        assert_eq!(expanded[1].node_id, id_a);
        assert!(expanded[0].full_content.contains("sess-depth2-b"));
        assert!(expanded[1].full_content.contains("sess-depth2-a"));
    }
}
