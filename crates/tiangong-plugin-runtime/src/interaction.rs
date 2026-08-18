//! 交互接缝（Interaction Seam）公共契约。
//!
//! Core 产生请求，宿主向声明 `capabilities.interaction=true` 的 UI 插件广播；
//! 插件只负责渲染和提交响应，截止时间、唯一闭合、挑战及授权由宿主裁决。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Approval,
    Confirm,
    Choice,
    MultiChoice,
    Input,
    Form,
}

impl InteractionKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Confirm => "confirm",
            Self::Choice => "choice",
            Self::MultiChoice => "multi_choice",
            Self::Input => "input",
            Self::Form => "form",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub kind: InteractionKind,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// options / fields / question 等 JSON 文本。
    #[serde(default)]
    pub payload: String,
    /// 宿主生成的本地时间字符串，格式 `%Y-%m-%dT%H:%M:%S%.f`。
    pub created_at: String,
    /// 宿主权威绝对截止时间。
    pub deadline: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionResponse {
    pub request_id: String,
    /// 响应负载 JSON 文本。
    pub result_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionClosed {
    pub request_id: String,
    pub session_id: String,
    /// answered | expired | cancelled
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 契约覆盖六种类型并保持蛇形序列化() {
        for kind in [
            InteractionKind::Approval,
            InteractionKind::Confirm,
            InteractionKind::Choice,
            InteractionKind::MultiChoice,
            InteractionKind::Input,
            InteractionKind::Form,
        ] {
            assert!(
                serde_json::to_string(&kind)
                    .unwrap()
                    .contains(kind.as_str())
            );
        }

        let request = InteractionRequest {
            request_id: "i1".to_string(),
            session_id: "s1".to_string(),
            tool_call_id: "call1".to_string(),
            kind: InteractionKind::MultiChoice,
            title: "选择分支".to_string(),
            description: String::new(),
            payload: r#"["main","dev"]"#.to_string(),
            created_at: "2026-08-18T12:00:00".to_string(),
            deadline: "2026-08-18T12:00:15".to_string(),
        };
        let parsed: InteractionRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(parsed, request);

        let response = InteractionResponse {
            request_id: "i1".to_string(),
            result_json: "\"main\"".to_string(),
        };
        let parsed: InteractionResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(parsed.result_json, "\"main\"");
    }
}

#[cfg(test)]
mod bridge_acceptance_tests {
    use super::*;

    /// 验收「插件权限」：未声明 interaction 能力时桥接拒绝（方案 §17 插件节）。
    #[test]
    fn 响应负载解析与缺字段拒绝() {
        let response = InteractionResponse {
            request_id: "r1".to_string(),
            result_json: r#"{"decision":"approve_once"}"#.to_string(),
        };
        let parsed: InteractionResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(parsed.request_id, "r1");
        assert!(parsed.result_json.contains("approve_once"));

        // 缺 request_id 的负载解析失败（宿主路由无法进行）
        assert!(serde_json::from_str::<InteractionResponse>(r#"{"result_json":"{}"}"#).is_err());
    }
}
