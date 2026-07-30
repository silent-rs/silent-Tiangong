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
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::{ToolCall, ToolSpec};
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
        }
    }
}

impl Plugin for WasmPluginAdapter {
    fn id(&self) -> &str {
        self.id
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

// PromptSectionProvider：阶段一示例插件不注入 prompt。
impl PromptSectionProvider for WasmPluginAdapter {}
