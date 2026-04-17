//! SQLite 加密连接与 CRUD 操作

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::types::{Episode, MemoryStatus};

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
    pub(crate) fn insert_episode(&self, episode: &Episode) -> Result<()> {
        let now = chrono::Local::now().naive_local().to_string();
        let keywords = serde_json::to_string(&episode.keywords)?;
        let tool_calls = serde_json::to_string(&episode.tool_calls)?;
        let outcome = serde_json::to_string(&episode.outcome)?;
        let full_content = serde_json::to_string(episode)?;

        // 写入 memory_nodes
        self.conn
            .execute(
                "INSERT OR REPLACE INTO memory_nodes
                 (id, kind, scope_type, scope_id, title, summary, keywords, importance,
                  confidence, status, source, usage_count, created_at, updated_at)
                 VALUES (?1, 'episode', 'workspace', NULL, ?2, ?3, ?4, ?5, 1.0, 'active', ?6, 0, ?7, ?7)",
                rusqlite::params![
                    episode.id,
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

    /// 查询最近的 Episode 摘要（用于 MesoRumination）
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
