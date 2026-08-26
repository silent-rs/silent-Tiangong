//! Desktop TypeScript 工具插件的进程内适配器。
//!
//! 适配器只把清单中的工具规格和提示词接入 Core，并把工具调用转交给
//! [`crate::ts_tools`]。工具参数和结果语义完全由插件 TypeScript 实现。

use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{
    MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
};

use crate::manifest::{PluginManifest, TsToolDecl};

#[derive(Clone, Default)]
struct TsPluginState {
    tools: Vec<TsToolDecl>,
    prompts: Vec<String>,
    /// @提及展示：候选 label（UI 贡献标题或插件 id）与副标题（mention.hint）。
    mention: Option<(String, String)>,
}

pub struct TsPluginAdapter {
    id: String,
    state: RwLock<TsPluginState>,
    enabled: AtomicBool,
    /// 无界面且声明 sidecar：工具由宿主直连 sidecar 执行（不进页面接应
    /// 协议）。有界面的插件照旧走页面路径（页面可深度参与执行）。
    sidecar_direct: AtomicBool,
}

impl TsPluginAdapter {
    pub(crate) fn from_manifest(manifest: &PluginManifest, enabled: bool) -> Self {
        Self {
            id: manifest.id.clone(),
            state: RwLock::new(TsPluginState {
                tools: manifest.tools.clone().unwrap_or_default(),
                prompts: manifest.prompt.clone().unwrap_or_default(),
                mention: mention_candidate_parts(manifest),
            }),
            enabled: AtomicBool::new(enabled),
            sidecar_direct: AtomicBool::new(sidecar_direct_of(manifest)),
        }
    }

    pub(crate) fn reconfigure(&self, manifest: &PluginManifest, enabled: bool) {
        let next = TsPluginState {
            tools: manifest.tools.clone().unwrap_or_default(),
            prompts: manifest.prompt.clone().unwrap_or_default(),
            mention: mention_candidate_parts(manifest),
        };
        match self.state.write() {
            Ok(mut state) => *state = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
        self.set_enabled(enabled);
        self.sidecar_direct
            .store(sidecar_direct_of(manifest), Ordering::Release);
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn tool(&self, name: &str) -> Option<TsToolDecl> {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }
}

impl Plugin for TsPluginAdapter {
    fn id(&self) -> &str {
        &self.id
    }
}

impl ToolSpecProvider for TsPluginAdapter {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            })
            .collect()
    }
}

impl ToolOverrideHandler for TsPluginAdapter {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        if !self.is_enabled() {
            return Box::pin(async { None });
        }
        let Some(tool) = self.tool(&call.name) else {
            return Box::pin(async { None });
        };
        let plugin_id = self.id.clone();
        let call = call.clone();
        if self.sidecar_direct.load(Ordering::Acquire) {
            let tool_timeout_ms = tool.timeout_ms;
            Box::pin(
                async move { Some(invoke_sidecar_tool(&plugin_id, call, tool_timeout_ms).await) },
            )
        } else {
            let session_id = session.id.clone();
            Box::pin(async move {
                Some(crate::ts_tools::execute(plugin_id, session_id, call, tool.timeout_ms).await)
            })
        }
    }
}

fn sidecar_direct_of(manifest: &PluginManifest) -> bool {
    manifest.ui_contributions().is_empty() && manifest.sidecar.is_some()
}

/// 无界面 sidecar 型插件的工具直达：宿主内置桥接，语义与 memory 等官方
/// 插件的 WASM 桥接层一致（透传）——operation 为工具名、参数为调用参数
/// 对象，sidecar 返回 ToolOutcome 形状（ok/summary/stdout/stderr/exit_code，
/// 后四项可缺省）。
///
/// 生命周期对齐页面接应路径：按工具声明的 `timeout_ms` 限时；超时或会话
/// 取消（Future 被 drop）时终止本次按需 sidecar 进程，不遗留阻塞调用。
async fn invoke_sidecar_tool(plugin_id: &str, call: ToolCall, timeout_ms: u64) -> ToolResult {
    let Some(directory) = crate::registry::plugin_install_directory(plugin_id) else {
        return sidecar_tool_failure(plugin_id, "插件未加载");
    };
    let Some(storage_root) = directory
        .parent()
        .and_then(|parent| parent.parent())
        .map(std::path::Path::to_path_buf)
    else {
        return sidecar_tool_failure(plugin_id, "无法定位插件存储根");
    };
    let installed = match crate::registry::find_installed_plugin(&storage_root, plugin_id) {
        Ok(installed) => installed,
        Err(error) => return sidecar_tool_failure(plugin_id, format!("{error:#}")),
    };
    let connection = match crate::registry::sidecar_connection(&storage_root, &installed, false) {
        Ok(connection) => connection,
        Err(error) => return sidecar_tool_failure(plugin_id, format!("{error:#}")),
    };
    // 进程守卫：调用完成（disarm）前，超时返回、panic 或会话取消（drop）
    // 都终止本次按需进程——后台 spawn_blocking 里的调用随进程断开而失败，
    // 不再运行到 sidecar 总超时。
    let mut guard = SidecarProcessGuard {
        connection: Some(connection.clone()),
    };
    let operation = call.name.clone();
    let timeout_operation = operation.clone();
    let arguments = call.arguments.clone();
    let blocking = connection.clone();
    let invoked = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::task::spawn_blocking(move || {
            blocking.invoke(
                &operation,
                &serde_json::to_string(&arguments).unwrap_or_default(),
            )
        }),
    )
    .await;
    let outcome = match invoked {
        // 工具级超时：guard drop 终止进程。
        Err(_) => Err(anyhow::anyhow!(
            "sidecar 工具 {timeout_operation} 超时（{timeout_ms}ms），已终止本次执行"
        )),
        Ok(Ok(Ok(raw))) => serde_json::from_str::<serde_json::Value>(&raw)
            .map_err(|error| anyhow::anyhow!("解析 sidecar 工具响应失败：{error}")),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(error)) => Err(anyhow::anyhow!("sidecar 调用任务失败：{error}")),
    };
    let result = match outcome {
        Ok(value) => ToolResult {
            ok: value
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            summary: value
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stdout: value
                .get("stdout")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stderr: value
                .get("stderr")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            exit_code: value
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(
                    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
                        1
                    } else {
                        0
                    },
                ) as i32,
            execution: None,
        },
        Err(error) => sidecar_tool_failure(plugin_id, format!("{error:#}")),
    };
    if result.ok {
        // 正常路径解除守卫：按需进程已由调用自身清理；常驻进程不受影响。
        // 超时/取消/panic 路径经 Drop 终止进程（后台阻塞调用随进程断开结束）。
        guard.connection.take();
    }
    result
}

struct SidecarProcessGuard {
    connection: Option<std::sync::Arc<dyn crate::sidecar::SidecarConnection>>,
}

impl Drop for SidecarProcessGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.cancel_current();
        }
    }
}

/// 测试通道：经真实直连路径调用（安装与阻塞超时验证）。
#[cfg(test)]
pub(crate) async fn invoke_sidecar_tool_for_test(
    plugin_id: &str,
    call: ToolCall,
    timeout_ms: u64,
) -> ToolResult {
    invoke_sidecar_tool(plugin_id, call, timeout_ms).await
}

fn sidecar_tool_failure(plugin_id: &str, message: impl std::fmt::Display) -> ToolResult {
    ToolResult {
        ok: false,
        summary: format!("插件 {plugin_id} sidecar 工具执行失败：{message}"),
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 1,
        execution: None,
    }
}

impl PromptSectionProvider for TsPluginAdapter {
    fn prompt_sections(&self) -> Vec<String> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .prompts
            .clone()
    }
}

impl MentionCandidateProvider for TsPluginAdapter {
    fn mention_candidates(&self) -> Vec<tiangong_core::MentionCandidate> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mention
            .as_ref()
            .map(|(label, hint)| {
                vec![tiangong_core::MentionCandidate {
                    value: format!("@plugin:{}", self.id),
                    label: label.clone(),
                    kind: "plugin".to_string(),
                    hint: hint.clone(),
                }]
            })
            .unwrap_or_default()
    }
}

/// 按清单静态生成 @提及候选（未声明 mention 返回 None）。
/// 供注册表实时聚合使用——TS 插件的候选是纯清单数据，不依赖适配器实例
///（适配器弱引用由会话 Core 构建时填充，安装后不存在）。
pub(crate) fn mention_candidate_from_manifest(
    manifest: &PluginManifest,
) -> Option<tiangong_core::MentionCandidate> {
    let (label, hint) = mention_candidate_parts(manifest)?;
    Some(tiangong_core::MentionCandidate {
        value: format!("@plugin:{}", manifest.id),
        label,
        kind: "plugin".to_string(),
        hint,
    })
}

/// 从清单推导 @提及候选的展示字段：label 取首个 UI 贡献标题（缺省插件 id），
/// hint 取 mention.hint（未声明 mention 则无候选）。
fn mention_candidate_parts(manifest: &PluginManifest) -> Option<(String, String)> {
    let mention = manifest.mention.as_ref()?;
    let label = manifest
        .ui
        .as_ref()
        .and_then(|ui| ui.contributions.first())
        .map(|contribution| {
            if contribution.title.is_empty() {
                manifest.id.clone()
            } else {
                contribution.title.clone()
            }
        })
        .unwrap_or_else(|| manifest.id.clone());
    Some((label, mention.hint.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MentionManifest;

    fn manifest_with_mention(hint: Option<&str>) -> PluginManifest {
        let mention = hint.map(|hint| {
            serde_json::from_str::<MentionManifest>(&format!(r#"{{"hint":"{hint}"}}"#)).unwrap()
        });
        PluginManifest {
            schema_version: 2,
            id: "demo".into(),
            version: "0.1.0".into(),
            wasm: None,
            sidecar: None,
            permissions: vec![],
            entrypoints: None,
            model_requirements: None,
            storage_access: false,
            capabilities: None,
            ui: Some(serde_json::from_str(
                r#"{"contributions":[{"slot":"extension.tab","id":"app","title":"演示插件","entry":"app/index.html"}]}"#,
            )
            .unwrap()),
            tools: None,
            prompt: None,
            resources: None,
            mention,
        }
    }

    #[test]
    fn mention候选_声明时生成_禁用时为空() {
        let adapter =
            TsPluginAdapter::from_manifest(&manifest_with_mention(Some("问候能力")), true);
        let candidates = MentionCandidateProvider::mention_candidates(&adapter);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "@plugin:demo");
        assert_eq!(candidates[0].label, "演示插件");
        assert_eq!(candidates[0].kind, "plugin");
        assert_eq!(candidates[0].hint, "问候能力");

        adapter.set_enabled(false);
        assert!(MentionCandidateProvider::mention_candidates(&adapter).is_empty());

        // 未声明 mention：无候选
        let adapter = TsPluginAdapter::from_manifest(&manifest_with_mention(None), true);
        assert!(MentionCandidateProvider::mention_candidates(&adapter).is_empty());
    }

    #[test]
    fn mention候选_无ui标题时用插件id() {
        let mut manifest = manifest_with_mention(Some("能力"));
        manifest.ui = None;
        let adapter = TsPluginAdapter::from_manifest(&manifest, true);
        let candidates = MentionCandidateProvider::mention_candidates(&adapter);
        assert_eq!(candidates[0].label, "demo");
    }
    #[test]
    fn 无界面sidecar插件_工具走直连_有界面走页面() {
        // 无 UI + sidecar：直连
        let mut manifest = manifest_with_mention(None);
        manifest.ui = None;
        manifest.sidecar =
            Some(serde_json::from_str(r#"{"runtime":"node","entry":"sidecar/main.mjs"}"#).unwrap());
        let adapter = TsPluginAdapter::from_manifest(&manifest, true);
        assert!(
            adapter
                .sidecar_direct
                .load(std::sync::atomic::Ordering::Acquire),
            "无界面 sidecar 插件应直连"
        );
        // 有 UI：照旧页面路径
        let mut manifest = manifest_with_mention(None);
        manifest.sidecar =
            Some(serde_json::from_str(r#"{"runtime":"node","entry":"sidecar/main.mjs"}"#).unwrap());
        let adapter = TsPluginAdapter::from_manifest(&manifest, true);
        assert!(
            !adapter
                .sidecar_direct
                .load(std::sync::atomic::Ordering::Acquire),
            "有界面插件应保持页面接应路径"
        );
        // 无 UI 无 sidecar：也不直连（无接应由 ts_tools 明确报错）
        let mut manifest = manifest_with_mention(None);
        manifest.ui = None;
        let adapter = TsPluginAdapter::from_manifest(&manifest, true);
        assert!(
            !adapter
                .sidecar_direct
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }
}
