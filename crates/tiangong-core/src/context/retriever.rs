//! 检索接口（预留）
//!
//! 为未来 RAG 能力提供统一的检索 trait。
//! 当前为空实现，后续可接入向量数据库、全文搜索等检索后端。

use anyhow::Result;

/// 检索结果
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// 检索内容
    pub content: String,
    /// 来源标识
    pub source: String,
    /// 相关性分数（0.0 ~ 1.0）
    pub score: f64,
}

/// 检索器 trait
///
/// 所有检索后端需实现此 trait，提供统一的检索接口。
pub trait Retriever: Send + Sync {
    /// 根据查询文本检索相关内容
    fn retrieve(&self, query: &str, top_k: usize) -> Result<Vec<RetrievalResult>>;
}

/// 空检索器（默认实现，不返回任何结果）
#[derive(Debug, Default)]
pub struct NoopRetriever;

impl Retriever for NoopRetriever {
    fn retrieve(&self, _query: &str, _top_k: usize) -> Result<Vec<RetrievalResult>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_retriever_returns_empty() {
        let retriever = NoopRetriever;
        let results = retriever.retrieve("test query", 5).unwrap();
        assert!(results.is_empty());
    }
}
