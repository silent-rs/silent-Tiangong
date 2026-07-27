use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value;
use tantivy::{Index, IndexWriter, TantivyDocument};

use super::tantivy_schema::{SessionFields, session_schema};

pub struct SessionIndex {
    index: Index,
    writer: IndexWriter,
    fields: SessionFields,
    session_id: String,
    turn_count: usize,
}

impl SessionIndex {
    pub fn open_or_create(session_id: &str, base_dir: &Path) -> Result<Self> {
        let index_dir = base_dir.join("sessions").join(session_id).join("tantivy");

        let (schema, fields) = session_schema();

        let index = if index_dir.exists() {
            Index::open_in_dir(&index_dir).context("打开 Session Tantivy 索引失败")?
        } else {
            fs::create_dir_all(&index_dir).context("创建 Session 索引目录失败")?;
            Index::create_in_dir(&index_dir, schema.clone())
                .context("创建 Session Tantivy 索引失败")?
        };

        let writer = index
            .writer(15_000_000)
            .context("创建 Session 索引写入器失败")?;

        Ok(Self {
            index,
            writer,
            fields,
            session_id: session_id.to_string(),
            turn_count: 0,
        })
    }

    pub fn index_turn(&mut self, turn: &super::TurnData) -> Result<()> {
        let importance = Self::estimate_importance(&turn.role, &turn.content);

        let mut doc = TantivyDocument::new();
        doc.add_text(self.fields.session_id, &self.session_id);
        doc.add_text(self.fields.workspace_id, &turn.workspace_id);
        doc.add_text(self.fields.turn_id, &turn.turn_id);
        doc.add_text(self.fields.content, &turn.content);
        doc.add_text(self.fields.role, &turn.role);
        doc.add_text(
            self.fields.timestamp,
            chrono::Local::now().naive_local().to_string(),
        );
        for topic in &turn.topics {
            doc.add_text(self.fields.topics, topic);
        }
        doc.add_f64(self.fields.importance, importance);
        for name in &turn.entity_names {
            doc.add_text(self.fields.entity_names, name);
        }

        self.writer.add_document(doc)?;
        self.turn_count += 1;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer.commit().context("提交 Session 索引失败")?;
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.commit()?;
        self.write_meta()
    }

    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<SessionSearchHit>> {
        let reader = self.index.reader().context("创建 Session 索引读取器失败")?;
        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![
                self.fields.content,
                self.fields.topics,
                self.fields.entity_names,
            ],
        );

        let query = query_parser
            .parse_query(query_text)
            .context("解析 Session 搜索查询失败")?;
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;

        let mut hits = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let turn_id = doc
                .get_first(self.fields.turn_id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let role = doc
                .get_first(self.fields.role)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = doc
                .get_first(self.fields.content)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            hits.push(SessionSearchHit {
                turn_id,
                role,
                content,
            });
        }
        Ok(hits)
    }

    pub fn turn_count(&self) -> usize {
        self.turn_count
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn estimate_importance(role: &str, content: &str) -> f64 {
        let mut importance: f64 = 0.5;
        if role == "assistant" {
            importance += 0.1;
        }
        if content.contains("error") || content.contains("Error") || content.contains("失败") {
            importance += 0.2;
        }
        if content.contains("tool_result") || content.contains("tool_use") {
            importance += 0.15;
        }
        importance.min(1.0)
    }

    fn write_meta(&self) -> Result<()> {
        let meta = SessionIndexMeta {
            session_id: self.session_id.clone(),
            turn_count: self.turn_count,
            updated_at: chrono::Local::now().naive_local().to_string(),
        };
        let meta_dir = self.meta_dir();
        fs::create_dir_all(&meta_dir).context("创建 Session meta 目录失败")?;
        let meta_path = meta_dir.join("meta.json");
        let json = serde_json::to_string_pretty(&meta).context("序列化 Session meta 失败")?;
        fs::write(&meta_path, json).context("写入 Session meta 失败")?;
        Ok(())
    }

    fn meta_dir(&self) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".tiangong")
            .join("index")
            .join("sessions")
            .join(&self.session_id)
    }
}

pub struct SessionSearchHit {
    pub turn_id: String,
    pub role: String,
    pub content: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionIndexMeta {
    session_id: String,
    turn_count: usize,
    updated_at: String,
}
