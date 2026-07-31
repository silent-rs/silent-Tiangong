//! WASM 插件 → 进程内 `Plugin` trait 适配器。
//!
//! [`WasmPluginAdapter`] 包装一个已实例化的 [`WasmPlugin`]，使其能被宿主当作
//! 原生插件注册。Wasmtime 的调用是同步阻塞的；适配器统一把处于 Tokio
//! runtime 内的调用转移到普通 OS 线程，避免 wasmtime-wasi 嵌套 `block_on`。
//!
//! 当前已转发插件描述、工具规格、工具调用、Prompt 段落和生命周期钩子。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::TrustMode;
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
use tokio::task;

use crate::config::PluginRuntimeConfig;
use crate::loader::WasmPlugin;

/// 把一个 WASM 插件实例包装为进程内 `Plugin`。
///
/// 注意：Wasmtime 的 Store 要求 `Send`（Engine 编译产物可跨线程），故
/// `Arc<Mutex<WasmPlugin>>` 可用于 `spawn_blocking`。
pub struct WasmPluginAdapter {
    inner: RwLock<Arc<Mutex<WasmPlugin>>>,
    config: PluginRuntimeConfig,
    /// 插件 id，构造期确定后不变。
    id: String,
    /// 反馈通道（每 turn 注入），供 handle 发送流事件。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    context: Mutex<ReloadContext>,
}

#[derive(Clone, Default)]
struct ReloadContext {
    workspace: Option<String>,
    config_json: Option<String>,
    session_json: Option<String>,
    trust_mode: Option<TrustMode>,
    exec_env: std::collections::BTreeMap<String, String>,
}

impl WasmPluginAdapter {
    pub fn new(plugin: WasmPlugin, config: PluginRuntimeConfig) -> Self {
        let inner = Arc::new(Mutex::new(plugin));
        let id_string = call_wasm_off_runtime(inner.clone(), |plugin| plugin.describe())
            .map(|d| d.id)
            .unwrap_or_else(|_| "wasm-unknown".to_string());
        Self {
            inner: RwLock::new(inner),
            id: id_string,
            config,
            feedback_tx: RwLock::new(None),
            context: Mutex::new(ReloadContext::default()),
        }
    }

    /// 返回内部 WasmPlugin 句柄的引用（供全局注册表查询 contributions/config）。
    pub fn inner_handle(&self) -> Arc<Mutex<WasmPlugin>> {
        self.current_inner()
    }

    pub(crate) fn runtime_config(&self) -> PluginRuntimeConfig {
        self.config.clone()
    }

    pub(crate) fn prepare_replacement(
        &self,
        mut plugin: WasmPlugin,
    ) -> anyhow::Result<Arc<Mutex<WasmPlugin>>> {
        let context = self
            .context
            .lock()
            .map_err(|_| anyhow::anyhow!("wasm 插件上下文锁已损坏"))?
            .clone();
        if let Some(config_json) = context.config_json {
            plugin.on_config_updated(config_json)?;
        }
        plugin.set_workspace(context.workspace)?;
        if let Some(session_json) = context.session_json {
            plugin.on_session_ready(session_json)?;
        }
        Ok(Arc::new(Mutex::new(plugin)))
    }

    pub(crate) fn replace_inner(&self, replacement: Arc<Mutex<WasmPlugin>>) {
        let mut current = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = replacement;
    }

    fn current_inner(&self) -> Arc<Mutex<WasmPlugin>> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Plugin for WasmPluginAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    /// 注入反馈通道（每 turn 注入），缓存供 handle 发送流事件。
    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    /// CoreConfig 变更：序列化为 JSON 转发到 WASM 组件的 on-config-updated。
    /// 序列化失败（不应发生）或 WASM 调用失败时仅记录 warning，不阻断 core 流程。
    fn on_config_updated(&self, config: &CoreConfig) {
        let config_json = match serde_json::to_string(config) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!("序列化 CoreConfig 失败，跳过 wasm 插件通知: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.config_json = Some(config_json.clone());
        }
        if let Err(e) =
            self.call_wasm_off_runtime(move |plugin| plugin.on_config_updated(config_json))
        {
            tracing::warn!("通知 wasm 插件配置变更失败: {e}");
        }
    }

    /// 会话就绪：序列化 session 只读快照转发到 WASM。
    fn on_session_ready(&self, session: &mut Session) {
        self.forward_session_hook(session, |plugin, json| plugin.on_session_ready(json));
    }

    /// 轮次开始：序列化 session 转发。
    fn on_turn_started(&self, session: &mut Session, turn_start_idx: usize) {
        self.forward_session_hook_with_idx(session, turn_start_idx, |plugin, json, idx| {
            plugin.on_turn_started(json, idx)
        });
    }

    /// 轮次结束：序列化 session 转发（WASM 内部触发 micro 反刍）。
    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        self.forward_session_hook_with_idx(session, turn_start_idx, |plugin, json, idx| {
            plugin.on_turn_finished(json, idx)
        });
    }

    /// 会话结束：序列化 session 转发（WASM 内部触发 meso 反刍）。
    fn on_session_ended(&self, session: &mut Session) {
        self.forward_session_hook(session, |plugin, json| plugin.on_session_ended(json));
    }

    /// 注入工作目录：传给 WASM 缓存，供 prompt_sections 拉注入用。
    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        let ws = workspace.map(|p| p.to_string_lossy().to_string());
        if let Ok(mut context) = self.context.lock() {
            context.workspace = ws.clone();
        }
        if let Err(e) = self.call_wasm_off_runtime(move |plugin| plugin.set_workspace(ws)) {
            tracing::warn!("wasm set_workspace 失败: {e}");
        }
    }

    fn set_trust_mode(&self, trust: TrustMode) {
        if let Ok(mut context) = self.context.lock() {
            context.trust_mode = Some(trust);
        }
    }

    fn set_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        if let Ok(mut context) = self.context.lock() {
            context.exec_env = env;
        }
    }
}

impl WasmPluginAdapter {
    /// 在 Tokio runtime 外执行 WASM 调用，避免 wasmtime-wasi 嵌套 block_on。
    fn call_wasm_off_runtime<R>(
        &self,
        call: impl FnOnce(&mut WasmPlugin) -> anyhow::Result<R> + Send,
    ) -> anyhow::Result<R>
    where
        R: Send,
    {
        call_wasm_off_runtime(self.current_inner(), call)
    }

    /// 序列化 session 只读快照，在独立线程调 WASM 钩子。失败仅 warn。
    fn forward_session_hook(
        &self,
        session: &Session,
        call: impl Fn(&mut WasmPlugin, String) -> anyhow::Result<()> + Send + Sync,
    ) {
        let json = match serde_json::to_string(session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 session 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        if let Err(e) = self.call_wasm_off_runtime(move |plugin| call(plugin, json)) {
            tracing::warn!("wasm 生命周期钩子失败: {e}");
        }
    }

    /// 同上，但带 turn_start_idx 参数。
    fn forward_session_hook_with_idx(
        &self,
        session: &Session,
        turn_start_idx: usize,
        call: impl Fn(&mut WasmPlugin, String, u32) -> anyhow::Result<()> + Send + Sync,
    ) {
        let json = match serde_json::to_string(session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 session 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        let idx = turn_start_idx as u32;
        if let Err(e) = self.call_wasm_off_runtime(move |plugin| call(plugin, json, idx)) {
            tracing::warn!("wasm 生命周期钩子失败: {e}");
        }
    }
}

// ToolSpecProvider：返回插件声明的工具规格。
impl ToolSpecProvider for WasmPluginAdapter {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        match self.call_wasm_off_runtime(WasmPlugin::tool_specs) {
            Ok(specs) => specs
                .into_iter()
                .map(|s| ToolSpec {
                    name: s.name,
                    description: s.description,
                    input_schema: serde_json::from_str(&s.input_schema).unwrap_or(Value::Null),
                })
                .collect(),
            Err(e) => {
                tracing::error!("读取 wasm 插件工具规格失败: {e}");
                Vec::new()
            }
        }
    }
}

// ToolOverrideHandler：处理插件工具调用（同步阻塞，移到 spawn_blocking）。
impl ToolOverrideHandler for WasmPluginAdapter {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut tiangong_core::session::Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        let arguments = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
        let wit_call = crate::loader::ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments,
        };
        let inner = self.current_inner();
        let config = self.config.clone();
        Box::pin(async move {
            let result = task::spawn_blocking(move || {
                call_wasm_off_runtime(inner, move |plugin| plugin.handle_tool(wit_call, &config))
            })
            .await
            .ok()?
            .ok()?;
            Some(ToolResult {
                ok: result.ok,
                summary: result.summary,
                stdout: result.stdout,
                stderr: result.stderr,
                exit_code: result.exit_code,
                execution: None,
            })
        })
    }
}

// PromptSectionProvider：调 WASM 的 prompt-sections 导出，拉取三级记忆注入。
impl PromptSectionProvider for WasmPluginAdapter {
    fn prompt_sections(&self) -> Vec<String> {
        match self.call_wasm_off_runtime(WasmPlugin::prompt_sections) {
            Ok(sections) => sections,
            Err(e) => {
                tracing::warn!("wasm prompt_sections 失败: {e}");
                Vec::new()
            }
        }
    }
}

pub(crate) fn call_wasm_off_runtime<R>(
    inner: Arc<Mutex<WasmPlugin>>,
    call: impl FnOnce(&mut WasmPlugin) -> anyhow::Result<R> + Send,
) -> anyhow::Result<R>
where
    R: Send,
{
    crate::execution::run_outside_tokio(move || {
        let mut plugin = inner
            .lock()
            .map_err(|e| anyhow::anyhow!("wasm 插件锁中毒: {e}"))?;
        call(&mut plugin)
    })
}
