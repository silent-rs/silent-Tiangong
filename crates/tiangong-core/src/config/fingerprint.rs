//! 执行配置指纹：标识「当前上下文是按哪套模型 + 工具声明积累出来的」。
//!
//! 指纹覆盖会影响历史可解释性的三件事：
//! - 模型标识（protocol + base_url + model）：换模型后旧历史的思考块、
//!   工具协议细节未必能被新模型正确续接；
//! - 工具名集合：工具声明变化会改写请求前缀，并让历史 tool_call 失去对应声明；
//! - 插件版本：升级后工具名往往不变，但行为、参数语义或返回结构可能已变，
//!   历史里按旧版本产生的调用与新版本不再一致（装卸载式升级尤其明显）。
//!
//! **不含任何凭据**：api_key、headers 一律不参与计算，也就不会写进会话文件。
//! 同理不含 reasoning_effort、trust_mode 等不影响历史结构的运行参数——那些
//! 改动下一轮直接生效，不需要交接。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tiangong_llm::ModelEndpoint;
use tiangong_llm::tool::ToolSpec;

/// 触发交接的原因（用于 notice 文案与日志，不参与指纹计算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigChangeKind {
    /// 仅模型变化。
    Model,
    /// 仅工具（插件）声明变化。
    Tools,
    /// 模型与工具同时变化。
    Both,
}

impl ConfigChangeKind {
    /// 面向用户的变化描述。
    pub fn describe(self) -> &'static str {
        match self {
            Self::Model => "模型",
            Self::Tools => "插件",
            Self::Both => "模型和插件",
        }
    }

    /// 合并多次变化：连续调整取并集，避免只记住最后一次。
    pub fn merge(self, other: Self) -> Self {
        if self == other { self } else { Self::Both }
    }
}

/// 待交接的配置变化标记。
///
/// 记录「目标指纹」而不是插件实例或完整工具声明：指纹是纯文本摘要，
/// 会话文件不会因此显著变大，也不会残留失效的插件引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingConfigHandoff {
    /// 变化后的目标指纹；压缩成功后写入 `Session::config_fingerprint`。
    pub fingerprint: String,
    /// 本次变化涉及的内容（多次调整已合并）。
    pub kind: ConfigChangeKind,
}

/// 执行配置指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFingerprint {
    model: String,
    tools: Vec<String>,
    /// 参与本轮声明的插件版本（`id@version`，已排序）。
    plugins: Vec<String>,
}

impl ConfigFingerprint {
    /// 按当前模型端点、工具声明与插件版本计算指纹输入。
    ///
    /// 工具名与插件标识排序后参与计算：加载顺序变化不改变能力集合，不应触发交接。
    pub fn of(endpoint: &ModelEndpoint, tools: &[ToolSpec], plugins: &[(&str, &str)]) -> Self {
        let mut names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        names.sort();
        names.dedup();
        // 版本参与指纹：升级后工具名常常不变，但行为可能已变，历史调用需要交接。
        // 无版本的插件（进程内插件、测试替身）只记 id，不会引入无谓变化。
        let mut plugin_keys: Vec<String> = plugins
            .iter()
            .map(|(id, version)| {
                if version.is_empty() {
                    (*id).to_string()
                } else {
                    format!("{id}@{version}")
                }
            })
            .collect();
        plugin_keys.sort();
        plugin_keys.dedup();
        Self {
            // 同名模型可能来自不同供应商或路由，故带上协议与 base_url。
            // api_key / headers 属凭据，绝不参与。
            model: format!(
                "{:?}|{}|{}",
                endpoint.protocol, endpoint.base_url, endpoint.model
            ),
            tools: names,
            plugins: plugin_keys,
        }
    }

    /// 摘要文本（写入会话文件的形态）。
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.model.as_bytes());
        for name in &self.tools {
            hasher.update(b"\x1f");
            hasher.update(name.as_bytes());
        }
        hasher.update(b"\x1e");
        for plugin in &self.plugins {
            hasher.update(b"\x1f");
            hasher.update(plugin.as_bytes());
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// 模型标识部分（不含凭据），供调用方判断是否换了模型。
    pub fn model_key(&self) -> &str {
        &self.model
    }

    /// 与既有指纹比较，返回变化种类；无变化返回 `None`。
    ///
    /// `previous` 是上一次定档时的完整指纹输入；只有摘要时无法区分变化来源，
    /// 调用方据此把 kind 记为 [`ConfigChangeKind::Both`]。
    pub fn diff(&self, previous: &Self) -> Option<ConfigChangeKind> {
        let model_changed = self.model != previous.model;
        // 插件版本变化与工具集合变化同属「插件」类：对用户是同一件事。
        let tools_changed = self.tools != previous.tools || self.plugins != previous.plugins;
        match (model_changed, tools_changed) {
            (false, false) => None,
            (true, false) => Some(ConfigChangeKind::Model),
            (false, true) => Some(ConfigChangeKind::Tools),
            (true, true) => Some(ConfigChangeKind::Both),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            model: model.to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-secret".to_string(),
            ..Default::default()
        }
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        }
    }

    /// 测试辅助：不关心插件版本的场景传空列表。
    fn fingerprint_of(endpoint: &ModelEndpoint, tools: &[ToolSpec]) -> ConfigFingerprint {
        ConfigFingerprint::of(endpoint, tools, &[])
    }

    #[test]
    fn 凭据不参与指纹计算() {
        let mut with_other_key = endpoint("gpt-4o");
        with_other_key.api_key = "sk-another".to_string();
        with_other_key
            .headers
            .insert("X-Token".to_string(), "secret".to_string());

        let base = fingerprint_of(&endpoint("gpt-4o"), &[tool("read_file")]);
        let other = fingerprint_of(&with_other_key, &[tool("read_file")]);

        assert_eq!(base.digest(), other.digest(), "换 api_key 不应触发交接");
        assert!(!base.digest().contains("sk-"), "摘要不得包含凭据原文");
    }

    #[test]
    fn 工具顺序变化不算能力变化() {
        let ordered = fingerprint_of(&endpoint("gpt-4o"), &[tool("a"), tool("b")]);
        let reversed = fingerprint_of(&endpoint("gpt-4o"), &[tool("b"), tool("a")]);
        assert_eq!(ordered.diff(&reversed), None);
    }

    #[test]
    fn 插件顺序变化不算能力变化() {
        let tools = [tool("a")];
        let ordered = ConfigFingerprint::of(
            &endpoint("gpt-4o"),
            &tools,
            &[("fs", "0.1.9"), ("terminal", "0.3.8")],
        );
        let reversed = ConfigFingerprint::of(
            &endpoint("gpt-4o"),
            &tools,
            &[("terminal", "0.3.8"), ("fs", "0.1.9")],
        );
        assert_eq!(ordered.diff(&reversed), None);
    }

    #[test]
    fn 插件升级即使工具名不变也触发交接() {
        // 升级常常保持工具名不变，但行为与参数语义可能已变：历史调用需要交接。
        let tools = [tool("read_file")];
        let old = ConfigFingerprint::of(&endpoint("gpt-4o"), &tools, &[("fs", "0.1.9")]);
        let upgraded = ConfigFingerprint::of(&endpoint("gpt-4o"), &tools, &[("fs", "0.2.0")]);

        assert_eq!(upgraded.diff(&old), Some(ConfigChangeKind::Tools));
        assert_ne!(upgraded.digest(), old.digest());
    }

    #[test]
    fn 无版本插件不引入无谓变化() {
        // 进程内插件与测试替身没有独立版本，只按 id 参与计算。
        let tools = [tool("a")];
        let first = ConfigFingerprint::of(&endpoint("gpt-4o"), &tools, &[("builtin", "")]);
        let second = ConfigFingerprint::of(&endpoint("gpt-4o"), &tools, &[("builtin", "")]);
        assert_eq!(first.diff(&second), None);
    }

    #[test]
    fn 区分模型与工具变化() {
        let base = fingerprint_of(&endpoint("gpt-4o"), &[tool("a")]);
        let model_only = fingerprint_of(&endpoint("claude"), &[tool("a")]);
        let tools_only = fingerprint_of(&endpoint("gpt-4o"), &[tool("a"), tool("b")]);
        let both = fingerprint_of(&endpoint("claude"), &[tool("b")]);

        assert_eq!(model_only.diff(&base), Some(ConfigChangeKind::Model));
        assert_eq!(tools_only.diff(&base), Some(ConfigChangeKind::Tools));
        assert_eq!(both.diff(&base), Some(ConfigChangeKind::Both));
        assert_eq!(base.diff(&base), None);
    }

    #[test]
    fn 多次变化合并为both() {
        assert_eq!(
            ConfigChangeKind::Model.merge(ConfigChangeKind::Tools),
            ConfigChangeKind::Both
        );
        assert_eq!(
            ConfigChangeKind::Model.merge(ConfigChangeKind::Model),
            ConfigChangeKind::Model
        );
    }
}
