//! 交互接缝（Interaction Seam）契约。
//!
//! 「agent 需要用户选择/填写/确认」的统一契约：ask_user 等交互发起方产生
//! [`InteractionRequest`]，默认交互 UI（或三方处理器）渲染并回传
//! [`InteractionResponse`]；超时与取消按拒绝闭合（fail-closed）。
//! 本模块只定形契约，不感知具体业务负载。

use serde::{Deserialize, Serialize};

/// 交互类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    /// 用户在候选中选择一项。
    Choice,
    /// 用户填写表单（字段 schema 为 JSON Schema 子集）。
    Form,
    /// 用户确认/否认。
    Confirm,
}

impl InteractionKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::Form => "form",
            Self::Confirm => "confirm",
        }
    }
}

/// 一次交互请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub interaction_id: String,
    /// 发起方插件 ID（内置 ask_user 为空）。
    #[serde(default)]
    pub plugin_id: String,
    pub kind: InteractionKind,
    pub title: String,
    /// 交互负载 JSON 文本：choice 为候选数组、form 为字段 schema、confirm 为问题。
    #[serde(default)]
    pub schema: String,
}

/// 一次交互响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionResponse {
    pub interaction_id: String,
    /// 响应负载 JSON 文本（choice 选项、form 字段对象、confirm true/false）。
    /// None 表示取消。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 契约序列化_snake_case() {
        let request = InteractionRequest {
            interaction_id: "i1".to_string(),
            plugin_id: String::new(),
            kind: InteractionKind::Choice,
            title: "选择分支".to_string(),
            schema: r#"["main","dev"]"#.to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"kind\":\"choice\""));
        let parsed: InteractionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, request);

        let response = InteractionResponse {
            interaction_id: "i1".to_string(),
            result: Some("\"main\"".to_string()),
        };
        let parsed: InteractionResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(parsed.result.as_deref(), Some("\"main\""));
    }
}
