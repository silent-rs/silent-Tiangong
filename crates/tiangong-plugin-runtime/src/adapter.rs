//! WASM 插件 → 进程内 `Plugin` trait 适配器。
//!
//! [`WasmPluginAdapter`] 包装一个已实例化的 [`WasmPlugin`]，使其能被宿主当作
//! 原生插件注册。Wasmtime 的调用是同步阻塞的；适配器统一把处于 Tokio
//! runtime 内的调用转移到普通 OS 线程，避免 wasmtime-wasi 嵌套 `block_on`。
//!
//! 当前已转发插件描述、工具规格、工具调用、Prompt 段落和生命周期钩子。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::permission::TrustMode;
use tiangong_core::session::Session;
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};
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
    enabled: AtomicBool,
    /// fire-and-forget 生命周期钩子的后台线程句柄。
    ///
    /// `on_turn_finished` / `on_session_ended` 投递到独立线程后立即返回，句柄存于此。
    /// [`join_pending_hooks`](Self::join_pending_hooks) 在会话关闭前回收，确保通知
    /// 尽量送达 sidecar，避免进程退出打断后台投递。
    pending_hooks: Mutex<Vec<std::thread::JoinHandle<()>>>,
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
        Self::new_with_enabled(plugin, config, true)
    }

    pub(crate) fn new_with_enabled(
        plugin: WasmPlugin,
        config: PluginRuntimeConfig,
        enabled: bool,
    ) -> Self {
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
            enabled: AtomicBool::new(enabled),
            pending_hooks: Mutex::new(Vec::new()),
        }
    }

    /// 使用预加载阶段已校验的插件 ID 构造实例，避免每个 Core 重复调用 describe。
    pub(crate) fn new_with_id(
        plugin: WasmPlugin,
        config: PluginRuntimeConfig,
        enabled: bool,
        id: String,
    ) -> Self {
        Self {
            inner: RwLock::new(Arc::new(Mutex::new(plugin))),
            id,
            config,
            feedback_tx: RwLock::new(None),
            context: Mutex::new(ReloadContext::default()),
            enabled: AtomicBool::new(enabled),
            pending_hooks: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// 等待所有 fire-and-forget 生命周期钩子（`on_turn_finished` / `on_session_ended`）
    /// 的后台投递完成。
    ///
    /// 这些钩子投递后立即返回，正常路径无需等待；但在会话关闭前调用本方法可确保
    /// 通知尽量送达 sidecar，避免进程退出打断后台投递。失败（线程 panic）仅记录，
    /// 不向上传播——这些钩子本就是 best-effort。
    pub fn join_pending_hooks(&self) {
        let handles = self
            .pending_hooks
            .lock()
            .map(|mut handles| handles.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in handles {
            if let Err(payload) = handle.join() {
                tracing::warn!(plugin_id = %self.id, "wasm 钩子后台线程异常: {payload:?}");
            }
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
        activate: bool,
    ) -> anyhow::Result<Arc<Mutex<WasmPlugin>>> {
        if !activate {
            return Ok(Arc::new(Mutex::new(plugin)));
        }
        let context = self
            .context
            .lock()
            .map_err(|_| anyhow::anyhow!("wasm 插件上下文锁已损坏"))?
            .clone();
        if let Ok(feedback) = self.feedback_tx.read()
            && let Some(feedback) = feedback.clone()
        {
            plugin.set_feedback(feedback);
        }
        if let Some(config_json) = context.config_json {
            plugin.on_config_updated(config_json)?;
        }
        plugin.set_workspace(context.workspace, self.is_full_trust())?;
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
            *guard = Some(tx.clone());
        }
        if let Err(error) = self.call_wasm_off_runtime(move |plugin| {
            plugin.set_feedback(tx);
            Ok(())
        }) {
            tracing::warn!(%error, "注入 wasm 插件反馈通道失败");
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
        if !self.is_enabled() {
            return;
        }
        if let Err(e) =
            self.call_wasm_off_runtime(move |plugin| plugin.on_config_updated(config_json))
        {
            tracing::warn!("通知 wasm 插件配置变更失败: {e}");
        }
    }

    /// 会话就绪：序列化 session 只读快照转发到 WASM。
    fn on_session_ready(&self, session: &mut Session) {
        self.forward_session_hook("on_session_ready", session, |plugin, json| {
            plugin.on_session_ready(json)
        });
    }

    /// 轮次开始：序列化 session 转发。
    fn on_turn_started(&self, session: &mut Session, turn_start_idx: usize) {
        self.forward_session_hook_with_idx(
            "on_turn_started",
            session,
            turn_start_idx,
            |plugin, json, idx| plugin.on_turn_started(json, idx),
        );
    }

    /// 轮次结束：序列化 session 转发（WASM 内部触发 micro 反刍）。
    ///
    /// 采用 fire-and-forget：WASM 调用（含 sidecar 往返）投递到独立 OS 线程，
    /// 调用方立即返回，避免把 sidecar 建连与往返开销堆在发布终态前的关键路径上。
    /// 反刍在 sidecar 端本就是后台异步执行，这里只需保证"通知发出"，不阻塞即可。
    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        self.forward_session_hook_with_idx_detached(
            "on_turn_finished",
            session,
            turn_start_idx,
            |plugin, json, idx| plugin.on_turn_finished(json, idx),
        );
    }

    /// 会话结束：序列化 session 转发（WASM 内部触发 meso 反刍）。
    ///
    /// 同样采用 fire-and-forget：投递到独立线程立即返回，不阻塞 finalize 关键路径。
    fn on_session_ended(&self, session: &mut Session) {
        self.forward_session_hook_detached("on_session_ended", session, |plugin, json| {
            plugin.on_session_ended(json)
        });
    }

    /// finalize 阶段由 Core 调用：等待 fire-and-forget 投递的后台钩子完成，
    /// 确保记忆反刍等通知在进程关闭前尽量送达 sidecar。
    fn join_pending_async(&self) {
        self.join_pending_hooks();
    }

    /// 注入工作目录：传给 WASM 缓存，供 prompt_sections 拉注入用。
    ///
    /// 同时携带当前信任模式（full_trust），供插件放宽/收紧工作区外路径校验。
    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        let ws = workspace.map(|p| p.to_string_lossy().to_string());
        if let Ok(mut context) = self.context.lock() {
            context.workspace = ws.clone();
        }
        if !self.is_enabled() {
            return;
        }
        let full_trust = self.is_full_trust();
        if let Err(e) =
            self.call_wasm_off_runtime(move |plugin| plugin.set_workspace(ws, full_trust))
        {
            tracing::warn!("wasm set_workspace 失败: {e}");
        }
    }

    fn set_trust_mode(&self, trust: TrustMode) {
        if let Ok(mut context) = self.context.lock() {
            context.trust_mode = Some(trust);
        }
        // 信任模式可能在 set_workspace 之后变更：重新调一次 set_workspace，
        // 把最新 trust 推送给 WASM（workspace 保持 context 中缓存的值不变）。
        if !self.is_enabled() {
            return;
        }
        let ws = self.context.lock().ok().and_then(|c| c.workspace.clone());
        let full_trust = matches!(trust, TrustMode::FullTrust);
        if let Err(e) =
            self.call_wasm_off_runtime(move |plugin| plugin.set_workspace(ws, full_trust))
        {
            tracing::warn!("wasm set_trust_mode 推送失败: {e}");
        }
    }

    fn set_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        if let Ok(mut context) = self.context.lock() {
            context.exec_env = env;
        }
    }
}

impl WasmPluginAdapter {
    /// 当前是否为完全信任模式（供 set_workspace 推送给 WASM）。
    fn is_full_trust(&self) -> bool {
        self.context.lock().ok().and_then(|c| c.trust_mode) == Some(TrustMode::FullTrust)
    }

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

    /// 序列化 PluginSession 只读快照，在独立线程调 WASM 钩子。失败仅 warn。
    fn forward_session_hook(
        &self,
        hook: &'static str,
        session: &Session,
        call: impl Fn(&mut WasmPlugin, String) -> anyhow::Result<()> + Send + Sync,
    ) {
        let plugin_session = tiangong_types::PluginSession::from(session);
        let json = match serde_json::to_string(&plugin_session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 PluginSession 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        if !self.is_enabled() {
            return;
        }
        if let Err(error) = self.call_wasm_off_runtime(move |plugin| call(plugin, json)) {
            tracing::warn!(plugin_id = %self.id, hook, %error, "wasm 生命周期钩子失败");
        }
    }

    /// 同上，但带 turn_start_idx 参数。
    fn forward_session_hook_with_idx(
        &self,
        hook: &'static str,
        session: &Session,
        turn_start_idx: usize,
        call: impl Fn(&mut WasmPlugin, String, u32) -> anyhow::Result<()> + Send + Sync,
    ) {
        let plugin_session = tiangong_types::PluginSession::from(session);
        let json = match serde_json::to_string(&plugin_session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 PluginSession 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        if !self.is_enabled() {
            return;
        }
        let idx = turn_start_idx as u32;
        if let Err(error) = self.call_wasm_off_runtime(move |plugin| call(plugin, json, idx)) {
            tracing::warn!(plugin_id = %self.id, hook, %error, "wasm 生命周期钩子失败");
        }
    }

    /// 与 [`forward_session_hook`](Self::forward_session_hook) 相同的序列化逻辑，
    /// 但把 WASM 调用投递到独立 OS 线程后立即返回，不阻塞调用方。
    ///
    /// 用于收尾类钩子（`on_turn_finished` / `on_session_ended`）：这类钩子只需保证
    /// 通知最终送达 sidecar，结果对当前路径无影响（失败本就只 warn），因此可以
    /// 完全脱离关键路径，避免 sidecar 建连与往返开销拖慢回复收尾。
    ///
    /// 使用 `std::thread::spawn` 而非 `tokio::spawn_blocking`，是为了在 session
    /// end 等 `spawn_blocking` 上下文中同样安全（那里没有 tokio runtime）。
    fn forward_session_hook_detached(
        &self,
        hook: &'static str,
        session: &Session,
        call: impl Fn(&mut WasmPlugin, String) -> anyhow::Result<()> + Send + Sync + 'static,
    ) {
        let plugin_session = tiangong_types::PluginSession::from(session);
        let json = match serde_json::to_string(&plugin_session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 PluginSession 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        if !self.is_enabled() {
            return;
        }
        let inner = self.current_inner();
        let plugin_id = self.id.clone();
        let hook_thread_id = plugin_id.clone();
        match std::thread::Builder::new()
            .name(format!("wasm-hook-{plugin_id}-{hook}"))
            .spawn(move || {
                if let Err(error) =
                    call_wasm_off_runtime(inner, move |plugin| call(plugin, json))
                {
                    tracing::warn!(plugin_id = %hook_thread_id, hook, %error, "wasm 生命周期钩子失败");
                }
            })
        {
            Ok(handle) => self.track_pending_hook(handle),
            Err(error) => {
                tracing::warn!(plugin_id = %plugin_id, hook, %error, "启动 wasm 钩子后台线程失败");
            }
        }
    }

    /// 同上，但带 turn_start_idx 参数。
    fn forward_session_hook_with_idx_detached(
        &self,
        hook: &'static str,
        session: &Session,
        turn_start_idx: usize,
        call: impl Fn(&mut WasmPlugin, String, u32) -> anyhow::Result<()> + Send + Sync + 'static,
    ) {
        let plugin_session = tiangong_types::PluginSession::from(session);
        let json = match serde_json::to_string(&plugin_session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 PluginSession 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        if let Ok(mut context) = self.context.lock() {
            context.session_json = Some(json.clone());
        }
        if !self.is_enabled() {
            return;
        }
        let inner = self.current_inner();
        let plugin_id = self.id.clone();
        let hook_thread_id = plugin_id.clone();
        let idx = turn_start_idx as u32;
        match std::thread::Builder::new()
            .name(format!("wasm-hook-{plugin_id}-{hook}"))
            .spawn(move || {
                if let Err(error) =
                    call_wasm_off_runtime(inner, move |plugin| call(plugin, json, idx))
                {
                    tracing::warn!(plugin_id = %hook_thread_id, hook, %error, "wasm 生命周期钩子失败");
                }
            })
        {
            Ok(handle) => self.track_pending_hook(handle),
            Err(error) => {
                tracing::warn!(plugin_id = %plugin_id, hook, %error, "启动 wasm 钩子后台线程失败");
            }
        }
    }

    /// 记录一个待回收的后台钩子句柄，供 [`join_pending_hooks`](Self::join_pending_hooks)
    /// 在会话关闭前统一等待。
    fn track_pending_hook(&self, handle: std::thread::JoinHandle<()>) {
        if let Ok(mut pending) = self.pending_hooks.lock() {
            pending.push(handle);
        }
    }
}

// ToolSpecProvider：返回插件声明的工具规格。
impl ToolSpecProvider for WasmPluginAdapter {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        if !self.is_enabled() {
            return Vec::new();
        }
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
        if !self.is_enabled() {
            return Box::pin(async { None });
        }
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
                execution: result.execution.map(|execution| ToolExecutionRecord {
                    tool_name: execution.tool_name,
                    args: execution.args,
                    duration_ms: execution.duration_ms,
                    ok: execution.ok,
                    exit_code: execution.exit_code,
                    summary: execution.summary,
                }),
            })
        })
    }
}

// PromptSectionProvider：调 WASM 的 prompt-sections 导出，拉取三级记忆注入。
impl PromptSectionProvider for WasmPluginAdapter {
    fn prompt_sections(&self) -> Vec<String> {
        if !self.is_enabled() {
            return Vec::new();
        }
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
