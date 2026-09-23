//! @提及请求与候选：Core 定义插件契约，CoreManager 负责上下文与收集。
use serde::{Deserialize, Serialize};

/// @提及候选项。
///
/// - `value`：插入值，如 `@skill:xxx` / `@mcp:yyy`，命中后写入输入框
/// - `label`：展示名
/// - `kind`：类型标签，如 `skill` / `mcp` / `agent`
/// - `hint`：副标题（描述、工具数等）
/// - `mark`：候选标记（chip 角标字符，如 `S` / `M`），由插件提供；
///   为空时前端按 `kind` 回退默认标记
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCandidate {
    pub value: String,
    pub label: String,
    pub kind: String,
    pub hint: String,
    #[serde(default)]
    pub mark: String,
}

/// @提及候选分组。
///
/// App 层按 `kind` 对插件提供的候选分组，供前端按组渲染（组标题 + 组内候选）。
/// - `kind`：分组类型标签，如 `skill` / `mcp` / `agent`
/// - `label`：组标题（展示用）
/// - `candidates`：组内候选（已按数量上限截断）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionGroup {
    pub kind: String,
    pub label: String,
    pub candidates: Vec<MentionCandidate>,
}

/// 输入目标不隐式回退：无效会话不能借用其他工作区。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MentionTarget {
    #[default]
    Global,
    Session {
        session_id: String,
    },
    Draft {
        workspace: String,
    },
}

/// 宿主解析后的本次查询上下文；不改变插件的会话执行状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionContext {
    pub session_id: Option<String>,
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MentionRequest {
    pub target: MentionTarget,
    pub query: String,
    pub allowed_kinds: Vec<String>,
    pub max_per_group: usize,
}

impl Default for MentionRequest {
    fn default() -> Self {
        Self {
            target: MentionTarget::Global,
            query: String::new(),
            allowed_kinds: Vec::new(),
            max_per_group: 50,
        }
    }
}

/// 传给插件的只读查询，query 匹配必须发生在数量截断之前。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionQuery {
    pub context: MentionContext,
    pub query: String,
    pub allowed_kinds: Vec<String>,
    pub max_per_group: usize,
}

/// 按查询词过滤候选：**词间 AND、字段间 OR**。
///
/// - 查询词：按空白切分并小写化；空查询不过滤（返回 `true`）
/// - 字段间 OR：同一个词命中 `label` / `value`（剥离 kind 前缀后）/ `hint`
///   任一字段即算命中
/// - 词间 AND：所有查询词都必须命中，允许不同词分布在不同字段
///
/// 静态清单候选过滤、宿主兜底、以及任何「来源未自行过滤」的补偿路径都共用
/// 本函数，保证同一输入在各来源上的判定一致。匹配必须发生在数量截断之前。
pub fn candidate_matches_query(candidate: &MentionCandidate, query: &str) -> bool {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect();
    if terms.is_empty() {
        return true;
    }
    let haystacks = [
        candidate.label.to_lowercase(),
        strip_kind_prefix(&candidate.value, &candidate.kind).to_lowercase(),
        candidate.hint.to_lowercase(),
    ];
    terms
        .iter()
        .all(|term| haystacks.iter().any(|hay| hay.contains(term.as_str())))
}

/// 剥离 `value` 的 kind 前缀：`@skill:foo` → `foo`。
///
/// 前缀形态为 `@<kind>:`；不匹配该形态时原样返回（如 `@index`、`@<role>`）。
/// 不剥离会让输入 `plugin` 命中所有 `@plugin:*` 候选。
fn strip_kind_prefix<'a>(value: &'a str, kind: &str) -> &'a str {
    let Some(rest) = value.strip_prefix('@') else {
        return value;
    };
    match rest.strip_prefix(kind).and_then(|r| r.strip_prefix(':')) {
        Some(rest) => rest,
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(value: &str, label: &str, kind: &str, hint: &str) -> MentionCandidate {
        MentionCandidate {
            value: value.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            hint: hint.to_string(),
            mark: String::new(),
        }
    }

    /// 空查询不过滤：刚唤出面板、尚未输入关键词时枚举型候选照常返回。
    #[test]
    fn 空查询不过滤() {
        let c = candidate("@plugin:demo", "演示插件", "plugin", "问候能力");
        assert!(candidate_matches_query(&c, ""));
        assert!(candidate_matches_query(&c, "   "));
    }

    /// 三个字段都参与匹配。
    #[test]
    fn 单词命中任一字段() {
        let c = candidate("@plugin:demo", "演示插件", "plugin", "问候能力");
        assert!(candidate_matches_query(&c, "演示")); // label
        assert!(candidate_matches_query(&c, "问候")); // hint
        assert!(candidate_matches_query(&c, "demo")); // value 剥离前缀
    }

    /// value 匹配前剥离 kind 前缀：否则输入 `plugin` 会命中所有插件候选。
    #[test]
    fn value匹配剥离kind前缀() {
        let c = candidate("@plugin:demo", "演示插件", "plugin", "问候能力");
        assert!(!candidate_matches_query(&c, "plugin"));
        // 连前缀一起输入也不命中：前缀已从匹配字段中剥离
        assert!(!candidate_matches_query(&c, "@plugin"));
        // 字面量候选（无冒号前缀）原样参与匹配
        let index = candidate("@index", "工作区搜索", "index", "搜索工作区文件");
        assert!(candidate_matches_query(&index, "index"));
        // `@<role>` 形态同样原样参与
        let role = candidate("@alice", "alice", "agent", "Agent");
        assert!(candidate_matches_query(&role, "alice"));
    }

    /// 词间 AND、字段间 OR：所有词都必须命中，同一个词命中任一字段即可。
    #[test]
    fn 多词按and匹配_字段间或() {
        let c = candidate("@plugin:demo", "演示插件", "plugin", "问候能力");
        // 两个词分属 label 与 hint，跨字段 AND 仍命中
        assert!(candidate_matches_query(&c, "演示 问候"));
        // 缺一个词即不命中
        assert!(!candidate_matches_query(&c, "演示 不存在"));
    }

    /// 大小写不敏感：查询词与字段都小写化后比较。
    #[test]
    fn 匹配忽略大小写() {
        let c = candidate("@plugin:demo", "Demo Plugin", "plugin", "Greet");
        assert!(candidate_matches_query(&c, "demo"));
        assert!(candidate_matches_query(&c, "DEMO"));
        assert!(candidate_matches_query(&c, "greet"));
    }
}
