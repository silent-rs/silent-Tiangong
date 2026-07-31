//! WASM 插件 → 进程内 `Plugin` trait 适配器。
//!
//! [`WasmPluginAdapter`] 包装一个已实例化的 [`WasmPlugin`]，使其能被宿主当作
//! 原生插件注册。Wasmtime 的调用是同步阻塞的（CPU 密集），适配器通过
//! `spawn_blocking` 把单次工具调用移出异步运行时，避免阻塞 executor。
//!
//! 阶段一 PoC：插件描述 / 工具规格 / 工具调用经此适配器；prompt 段落与生命周期
//! 钩子暂用默认空实现（示例 memory 插件不注入 prompt、不订阅生命周期）。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::{ToolCall, ToolSpec};
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
    inner: Arc<Mutex<WasmPlugin>>,
    config: PluginRuntimeConfig,
    /// 插件 id，构造期确定后不变。泄漏为 'static 以满足 `Plugin::id -> &str`。
    id: &'static str,
    /// 反馈通道（每 turn 注入），供 handle 发送流事件。
    feedback_tx: std::sync::RwLock<Option<PluginFeedbackTx>>,
}

impl WasmPluginAdapter {
    pub fn new(mut plugin: WasmPlugin, config: PluginRuntimeConfig) -> Self {
        let id_string = plugin
            .describe()
            .map(|d| d.id)
            .unwrap_or_else(|_| "wasm-unknown".to_string());
        // describe 已消耗一次调用，handle 会重置 fuel，无需处理。
        let id: &'static str = Box::leak(id_string.into_boxed_str());
        let _ = &mut plugin;
        Self {
            inner: Arc::new(Mutex::new(plugin)),
            id,
            config,
            feedback_tx: std::sync::RwLock::new(None),
        }
    }

    /// 返回内部 WasmPlugin 句柄的引用（供全局注册表查询 contributions/config）。
    pub fn inner_handle(&self) -> Arc<Mutex<WasmPlugin>> {
        self.inner.clone()
    }
}

impl Plugin for WasmPluginAdapter {
    fn id(&self) -> &str {
        self.id
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
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return;
            }
        };
        if let Err(e) = plugin.on_config_updated(config_json) {
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
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return;
            }
        };
        if let Err(e) = plugin.set_workspace(ws) {
            tracing::warn!("wasm set_workspace 失败: {e}");
        }
    }
}

impl WasmPluginAdapter {
    /// 序列化 session 只读快照，调用 WASM 钩子。失败仅 warn，不阻断 core。
    fn forward_session_hook(
        &self,
        session: &Session,
        call: impl Fn(&mut WasmPlugin, String) -> anyhow::Result<()>,
    ) {
        let json = match serde_json::to_string(session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 session 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return;
            }
        };
        if let Err(e) = call(&mut plugin, json) {
            tracing::warn!("wasm 生命周期钩子失败: {e}");
        }
    }

    /// 同上，但带 turn_start_idx 参数。
    fn forward_session_hook_with_idx(
        &self,
        session: &Session,
        turn_start_idx: usize,
        call: impl Fn(&mut WasmPlugin, String, u32) -> anyhow::Result<()>,
    ) {
        let json = match serde_json::to_string(session) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("序列化 session 失败，跳过 wasm 钩子: {e}");
                return;
            }
        };
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return;
            }
        };
        if let Err(e) = call(&mut plugin, json, turn_start_idx as u32) {
            tracing::warn!("wasm 生命周期钩子失败: {e}");
        }
    }
}

// ToolSpecProvider：返回插件声明的工具规格。
impl ToolSpecProvider for WasmPluginAdapter {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return Vec::new();
            }
        };
        match plugin.tool_specs() {
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
        let inner = self.inner.clone();
        let config = self.config.clone();
        Box::pin(async move {
            let result = task::spawn_blocking(move || {
                let mut plugin = inner.lock().ok()?;
                plugin.handle_tool(wit_call, &config).ok()
            })
            .await
            .ok()??;
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
        let mut plugin = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("wasm 插件锁中毒: {e}");
                return Vec::new();
            }
        };
        match plugin.prompt_sections() {
            Ok(sections) => sections,
            Err(e) => {
                tracing::warn!("wasm prompt_sections 失败: {e}");
                Vec::new()
            }
        }
    }
}
