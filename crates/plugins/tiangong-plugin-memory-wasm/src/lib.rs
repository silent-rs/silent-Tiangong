//! Memory System 的 WASM 组件（阶段二）。
//!
//! 在阶段一基础上，把记忆系统的纯逻辑（融合重排、检索锚点规划、召回结果整理）
//! 下沉到本组件内，调用时走「无 LLM 降级路径」自行产出结果：
//! - `recall_memory` 工具内部：规则规划锚点 → 用 mock 命中数据 → 规则整理；
//! - 经 `clock` host import 获取真实时间（替代 `chrono::Local::now()`）。
//!
//! 仍不接入任何存储或模型 host import（阶段三）。

mod anchor;
mod bindings;
mod rerank;
mod synthesize;
mod text_utils;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, MemoryKind, PlannedRecall, PluginDescriptor, PluginError, RecallAnchors, RecallHit,
    SearchStrategy, ToolCall, ToolResult, ToolSpec,
};
use bindings::tiangong::plugin::{clock, memory_store};

mod descriptor {
    pub const ID: &str = "memory";
    pub const NAME: &str = "Memory";
    pub const VERSION: &str = "0.3.0";
}

/// recall_memory 工具的 input_schema（JSON 文本）。
/// 与进程内版本 `tiangong-plugin-memory/src/handler.rs` 保持一致。
const RECALL_MEMORY_INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
    },
    "reason": {
      "type": "string",
      "description": "为什么需要回忆，简述当前任务依赖的历史语境"
    },
    "expected": {
      "type": "array",
      "items": { "type": "string" },
      "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
    },
    "limit": {
      "type": "integer",
      "description": "最多返回多少条记忆，默认 5，最大 10"
    }
  },
  "required": ["query"]
}"#;

const RECALL_MEMORY_DESCRIPTION: &str = "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。";

/// WASM 组件主体。阶段二仍为无状态组件（无存储）。
struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: descriptor::ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: descriptor::VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(vec![ToolSpec {
            name: "recall_memory".to_string(),
            description: RECALL_MEMORY_DESCRIPTION.to_string(),
            input_schema: RECALL_MEMORY_INPUT_SCHEMA.to_string(),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        if call.name != "recall_memory" {
            return Err(PluginError::Message(format!(
                "memory 组件不支持工具: {}",
                call.name
            )));
        }

        // 阶段二：在 WASM 内走降级路径（无存储/无 LLM）。
        let query = parse_query(&call.arguments).unwrap_or_default();
        let reason = parse_string_field(&call.arguments, "reason");
        let limit = parse_u32_field(&call.arguments, "limit")
            .unwrap_or(5)
            .clamp(1, 10);
        let expected: Vec<String> = parse_string_array(&call.arguments, "expected");

        // 1. 规则规划检索锚点（下沉的 fallback_plan）。
        let planned = anchor::fallback_plan(
            &anchor::RecallInput {
                query: &query,
                reason: reason.as_deref(),
                expected: &expected,
                context: &[],
            },
            limit,
        );

        // strategy 为 Skip 时直接返回无需回忆。
        if matches!(planned.anchors.strategy, Some(SearchStrategy::Skip)) {
            return Ok(tool_result_ok(format!(
                "当前请求「{query}」无需历史上下文（规则判定 Skip）。"
            )));
        }

        // 2. 经 memory-store host import 查询真实记忆（BM25 粗召回）。
        //    宿主未注入 MemoryHandle 时返回 disabled，回退到 mock 命中。
        //    wit-bindgen 对 import/export 生成了两份 RecallHit，需逐字段转换。
        let store_hits: Vec<RecallHit> =
            match memory_store::recall(&planned.anchors.query, &planned.anchors.keywords, limit) {
                Ok(resp) if !resp.hits.is_empty() => {
                    resp.hits.into_iter().map(convert_hit).collect()
                }
                _ => mock_recall_hits(&query),
            };

        // 3. 规则整理召回结果（下沉的 fallback_synthesize）。
        let content = synthesize::fallback_synthesize(&query, &[], &store_hits);

        // 4. 经 clock host import 获取真实时间戳，附在结果中证明 host import 生效。
        let now_ms = clock::now_millis();
        let summary = format!(
            "{content}\n\n[recall at t={now_ms}ms, strategy={:?}]",
            planned.anchors.strategy
        );

        Ok(tool_result_ok(summary))
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    // ── 阶段二：下沉的纯逻辑导出 ──

    fn rerank_fuse(
        bm25: Vec<RecallHit>,
        semantic: Vec<RecallHit>,
        semantic_ratio: f64,
        limit: u32,
    ) -> Result<Vec<RecallHit>, PluginError> {
        let reranker = rerank::Reranker::from_semantic_ratio(semantic_ratio.clamp(0.0, 1.0));
        Ok(reranker.fuse(bm25, semantic, limit as usize))
    }

    fn plan_recall_fallback(
        query: String,
        reason: Option<String>,
        expected: Vec<String>,
        context: Vec<String>,
        limit: u32,
    ) -> Result<PlannedRecall, PluginError> {
        let planned = anchor::fallback_plan(
            &anchor::RecallInput {
                query: &query,
                reason: reason.as_deref(),
                expected: &expected,
                context: &context,
            },
            limit,
        );
        Ok(PlannedRecall {
            anchors: RecallAnchors {
                query: planned.anchors.query,
                keywords: planned.anchors.keywords,
                strategy: planned.anchors.strategy,
            },
            limit: planned.limit,
            used_llm: planned.used_llm,
        })
    }

    fn synthesize_fallback(
        query: String,
        context: Vec<String>,
        hits: Vec<RecallHit>,
    ) -> Result<String, PluginError> {
        Ok(synthesize::fallback_synthesize(&query, &context, &hits))
    }

    fn store_write_episode(
        episode_json: String,
        workspace_id: Option<String>,
    ) -> Result<(), PluginError> {
        // 经 memory-store host import 写入；宿主无 handle 时返回 disabled，转为 plugin-error。
        memory_store::write_episode(&episode_json, workspace_id.as_deref()).map_err(|e| match e {
            memory_store::MemoryStoreError::Message(m) => PluginError::Message(m),
            memory_store::MemoryStoreError::Disabled => {
                PluginError::Message("memory-store 未注入 handle".to_string())
            }
        })
    }

    fn store_upsert_manual_memory(draft_json: String) -> Result<String, PluginError> {
        memory_store::upsert_manual_memory(&draft_json).map_err(|e| match e {
            memory_store::MemoryStoreError::Message(m) => PluginError::Message(m),
            memory_store::MemoryStoreError::Disabled => {
                PluginError::Message("memory-store 未注入 handle".to_string())
            }
        })
    }

    fn on_config_updated(config_json: String) -> Result<(), PluginError> {
        // 通用配置变更事件。memory 组件当前仅记录日志级别的影响，
        // 真正的 memory 配置由宿主侧 MemoryConfig 独立管理。
        // 解析失败不阻断（向前兼容：宿主可能传入插件不认识的字段）。
        if config_json.trim().is_empty() {
            return Ok(());
        }
        // 阶段性实现：确认能收到 config 事件即可。
        Ok(())
    }
}

fn tool_result_ok(summary: String) -> ToolResult {
    ToolResult {
        ok: true,
        summary,
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    }
}

/// 生成与 query 相关的 mock 召回命中（阶段二无真实存储）。
fn mock_recall_hits(query: &str) -> Vec<RecallHit> {
    vec![
        RecallHit {
            node_id: "mock-1".to_string(),
            title: format!("关于「{query}」的历史讨论"),
            summary: format!(
                "此前曾就「{query}」展开过讨论，记录了关键结论与相关文件 ./docs/{query}.md。"
            ),
            score: 0.9,
            kind: MemoryKind::Episode,
            importance: 0.8,
            depth1_loaded: false,
        },
        RecallHit {
            node_id: "mock-2".to_string(),
            title: format!("「{query}」相关的决定"),
            summary: format!(
                "基于「{query}」做出的技术决定，参考 https://example.invalid/{query}。"
            ),
            score: 0.7,
            kind: MemoryKind::Decision,
            importance: 0.6,
            depth1_loaded: false,
        },
    ]
}

/// 把 memory-store（import 侧）的 RecallHit 转成 plugin（export 侧）的 RecallHit。
///
/// wit-bindgen 对 import 和 export 分别生成类型，跨边界的 recall-hit 需逐字段转换。
fn convert_hit(h: bindings::tiangong::plugin::plugin::RecallHit) -> RecallHit {
    use bindings::tiangong::plugin::plugin::MemoryKind as ImportKind;
    RecallHit {
        node_id: h.node_id,
        title: h.title,
        summary: h.summary,
        score: h.score,
        kind: match h.kind {
            ImportKind::Episode => MemoryKind::Episode,
            ImportKind::Entity => MemoryKind::Entity,
            ImportKind::Decision => MemoryKind::Decision,
            ImportKind::Evidence => MemoryKind::Evidence,
        },
        importance: h.importance,
        depth1_loaded: h.depth1_loaded,
    }
}

// ── 最小 JSON 解析（避免引入 serde 依赖） ──

/// 从 arguments（JSON 文本）中解析 query 字段。
fn parse_query(arguments: &str) -> Option<String> {
    parse_string_field(arguments, "query")
}

fn parse_string_field(arguments: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let idx = arguments.find(&key)?;
    let after_key = &arguments[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let quote = after_colon.find('"')?;
    let value_start = quote + 1;
    let value_rest = &after_colon[value_start..];
    let end_quote = value_rest.find('"')?;
    Some(value_rest[..end_quote].to_string())
}

fn parse_u32_field(arguments: &str, field: &str) -> Option<u32> {
    let key = format!("\"{field}\"");
    let idx = arguments.find(&key)?;
    let after_key = &arguments[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = &after_key[colon + 1..];
    let digits: String = after_colon
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn parse_string_array(arguments: &str, field: &str) -> Vec<String> {
    let key = format!("\"{field}\"");
    let Some(idx) = arguments.find(&key) else {
        return Vec::new();
    };
    let after_key = &arguments[idx + key.len()..];
    let Some(open) = after_key.find('[') else {
        return Vec::new();
    };
    let rest = &after_key[open + 1..];
    let close = rest.find(']').unwrap_or(rest.len());
    let body = &rest[..close];
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '"' {
            chars.next();
            let mut s = String::new();
            for cc in chars.by_ref() {
                if cc == '"' {
                    break;
                }
                s.push(cc);
            }
            if !s.is_empty() {
                out.push(s);
            }
        } else {
            chars.next();
        }
    }
    out
}

bindings::export!(Component with_types_in bindings);
