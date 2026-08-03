//! WASM 组件加载器。
//!
//! 负责：
//! - 管理 App 级共享的 [`Engine`]（编译缓存复用）和 [`Linker`]（host import 注册）；
//! - 编译单文件 `.wasm` Component（重型操作，每插件生命周期内只调一次）；
//! - 从已编译 Component 创建独立 Store + 实例（轻量，每个 Core 调一次）。
//!
//! Engine 和 Linker 是 App 级单例（`OnceLock`），所有插件共享同一份编译基础设施。
//! 每个插件编译后的 [`Component`] 缓存在 registry 的 `LoadedPlugin` 中，
//! 创建 Core 实例时直接复用，不再重复编译。

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};

use crate::bindings::TiangongPlugin;
use crate::bindings::exports::tiangong::plugin::plugin::{PluginError, ToolCall as WitToolCall};
use crate::config::PluginRuntimeConfig;
use crate::host_state::HostState;
use crate::sidecar::SidecarConnection;

/// App 级共享 Wasmtime Engine（编译缓存复用）。
static SHARED_ENGINE: OnceLock<Engine> = OnceLock::new();

/// App 级共享 Linker（host import 注册）。
static SHARED_LINKER: OnceLock<Arc<Linker<HostState>>> = OnceLock::new();

/// host import 的 host_getter：返回 HostState 自身的可变借用。
fn host_self_getter(state: &mut HostState) -> &mut HostState {
    state
}

/// 获取共享 Engine（首次调用时初始化）。
pub fn shared_engine() -> &'static Engine {
    SHARED_ENGINE.get_or_init(|| {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        cfg.strategy(wasmtime::Strategy::Cranelift);
        Engine::new(&cfg).expect("创建 Wasmtime Engine 失败")
    })
}

/// 获取共享 Linker（首次调用时初始化）。
pub fn shared_linker() -> &'static Arc<Linker<HostState>> {
    SHARED_LINKER.get_or_init(|| {
        let engine = shared_engine();
        let mut linker = Linker::<HostState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("接入 WASI 失败");
        crate::bindings::tiangong::plugin::clock::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_self_getter,
        )
        .expect("接入 clock 失败");
        crate::bindings::tiangong::plugin::sidecar::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_self_getter,
        )
        .expect("接入 sidecar 失败");
        crate::bindings::tiangong::plugin::feedback::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_self_getter,
        )
        .expect("接入 feedback 失败");
        Arc::new(linker)
    })
}

/// 编译 WASM 字节为 Component（重型操作）。
///
/// 使用共享 Engine 编译。每个插件生命周期内只需编译一次，结果缓存在 registry
/// 的 `LoadedPlugin.component` 中。后续创建 Core 实例时复用此 Component，不再重复编译。
pub fn compile_component(bytes: &[u8]) -> Result<Component> {
    let t = std::time::Instant::now();
    let component = Component::new(shared_engine(), bytes)
        .map_err(|e| anyhow::anyhow!("编译 wasm 组件失败: {e}"))?;
    tracing::info!(
        target: "perf_trace",
        stage = "wasm.component.compile",
        elapsed_ms = t.elapsed().as_millis() as u64,
        "性能跟踪"
    );
    Ok(component)
}

/// 从已编译 Component 创建独立 Store + 实例（轻量操作）。
///
/// 每个 Core 调用一次，创建独立的 Store（含独立内存/fuel/epoch）和组件实例。
/// 不涉及编译，预期耗时几毫秒。
pub fn instantiate_component(
    component: &Component,
    config: &PluginRuntimeConfig,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    plugin_id: &str,
) -> Result<WasmPlugin> {
    let t = std::time::Instant::now();
    let limits = StoreLimitsBuilder::new()
        .memory_size(config.memory_limit)
        .build();

    let mut store = Store::new(
        shared_engine(),
        HostState::new(limits, sidecar, plugin_id.to_string()),
    );
    store.limiter(|state: &mut HostState| state.limits_mut());
    let _ = store.set_fuel(config.fuel_limit);
    store.set_epoch_deadline(u64::MAX);
    tracing::info!(
        target: "perf_trace",
        plugin_id,
        stage = "wasm.store.create",
        elapsed_ms = t.elapsed().as_millis() as u64,
        "性能跟踪"
    );

    let t = std::time::Instant::now();
    let instance = TiangongPlugin::instantiate(&mut store, component, shared_linker())
        .map_err(|e| anyhow::anyhow!("实例化 wasm 组件失败: {e}"))?;
    tracing::info!(
        target: "perf_trace",
        plugin_id,
        stage = "wasm.component.instantiate",
        elapsed_ms = t.elapsed().as_millis() as u64,
        "性能跟踪"
    );

    Ok(WasmPlugin { instance, store })
}

/// WASM 组件加载器（向后兼容薄包装）。
///
/// Engine 和 Linker 已改为 App 级共享（[`shared_engine`] / [`shared_linker`]）。
/// 本结构体仅保留 `sidecar` 字段，供 `load_wasm_plugin_at` 等测试入口使用。
pub struct WasmPluginLoader {
    sidecar: Option<Arc<dyn SidecarConnection>>,
}

impl WasmPluginLoader {
    /// 以给定配置创建加载器，不注入 sidecar（invoke 返回 unavailable）。
    pub fn new(_config: &PluginRuntimeConfig) -> Result<Self> {
        Self::with_sidecar(_config, None)
    }

    /// 以给定配置创建加载器，并注入 sidecar 连接。
    ///
    /// Engine 和 Linker 已改为 App 级共享，这里只确保它们已初始化并注入 sidecar。
    pub fn with_sidecar(
        _config: &PluginRuntimeConfig,
        sidecar: Option<Arc<dyn SidecarConnection>>,
    ) -> Result<Self> {
        // 确保 Engine 和 Linker 已初始化（首次调用时触发）。
        let _ = shared_engine();
        let _ = shared_linker();
        Ok(Self { sidecar })
    }

    /// 引擎句柄（共享 Engine 的引用）。
    pub fn engine(&self) -> &'static Engine {
        shared_engine()
    }

    /// 加载并实例化一个 `.wasm` Component，返回宿主侧 [`WasmPlugin`]。
    pub fn load(&self, wasm_path: &Path, config: &PluginRuntimeConfig) -> Result<WasmPlugin> {
        let plugin_id = wasm_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("plugin");
        self.load_for_plugin(wasm_path, config, plugin_id)
    }

    /// 使用清单中的插件 ID 加载并实例化一个 `.wasm` Component。
    pub fn load_for_plugin(
        &self,
        wasm_path: &Path,
        config: &PluginRuntimeConfig,
        plugin_id: &str,
    ) -> Result<WasmPlugin> {
        let bytes = std::fs::read(wasm_path).map_err(|e| {
            anyhow::anyhow!("读取 wasm 组件失败 {path}: {e}", path = wasm_path.display())
        })?;
        self.load_bytes_for_plugin(&bytes, config, plugin_id)
            .map_err(|error| {
                anyhow::anyhow!(
                    "加载 wasm 组件失败 {path}: {error}",
                    path = wasm_path.display()
                )
            })
    }

    /// 从字节编译 + 实例化（向后兼容组合方法）。
    ///
    /// 内部调 [`compile_component`] + [`instantiate_component`]。
    /// 仅供 `load_wasm_plugin_at` 等测试入口使用；生产路径应分别调 compile 和
    /// instantiate 以复用编译缓存。
    pub fn load_bytes_for_plugin(
        &self,
        bytes: &[u8],
        config: &PluginRuntimeConfig,
        plugin_id: &str,
    ) -> Result<WasmPlugin> {
        let component = compile_component(bytes)?;
        instantiate_component(&component, config, self.sidecar.clone(), plugin_id)
    }
}

/// 宿主侧持有的已实例化 WASM 插件句柄。
///
/// 每个实例拥有独立的 [`Store`]（含独立线性内存、fuel、epoch），
/// 但共享底层 Engine 和编译产物（Component 的 JIT 代码）。
pub struct WasmPlugin {
    instance: TiangongPlugin,
    store: Store<HostState>,
}

impl WasmPlugin {
    pub fn set_feedback(&mut self, feedback: tiangong_core::core::plugin::PluginFeedbackTx) {
        self.store.data_mut().set_feedback(feedback);
    }

    /// 插件描述符。
    pub fn describe(&mut self) -> Result<Descriptor> {
        self.instance
            .tiangong_plugin_plugin()
            .call_describe(&mut self.store)
            .map_err(|e| anyhow::anyhow!("describe 调用失败: {e}"))?
            .map_err(plugin_err)
            .map(|d| Descriptor {
                id: d.id,
                name: d.name,
                version: d.version,
            })
    }

    /// 插件声明的工具规格（JSON Schema 仍为文本）。
    pub fn tool_specs(&mut self) -> Result<Vec<Spec>> {
        self.instance
            .tiangong_plugin_plugin()
            .call_tool_specs(&mut self.store)
            .map_err(|e| anyhow::anyhow!("tool-specs 调用失败: {e}"))?
            .map_err(plugin_err)
            .map(|specs| {
                specs
                    .into_iter()
                    .map(|s| Spec {
                        name: s.name,
                        description: s.description,
                        input_schema: s.input_schema,
                    })
                    .collect()
            })
    }

    /// 插件贡献的 prompt 段落。
    pub fn prompt_sections(&mut self) -> Result<Vec<String>> {
        self.instance
            .tiangong_plugin_plugin()
            .call_prompt_sections(&mut self.store)
            .map_err(|e| anyhow::anyhow!("prompt-sections 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 在施加资源限制的前提下处理一次工具调用。
    pub fn handle_tool(&mut self, call: ToolCall, limits: &PluginRuntimeConfig) -> Result<Outcome> {
        // 单次调用前重置 fuel 与 epoch deadline。
        // set_fuel 仅在未开启 consume_fuel 时返回 Err，配置已开启，安全忽略。
        let _ = self.store.set_fuel(limits.fuel_limit);
        self.store.set_epoch_deadline(limits.epoch_deadline_ticks());

        let wit_call = WitToolCall {
            id: call.id,
            name: call.name,
            arguments: call.arguments,
        };

        match self
            .instance
            .tiangong_plugin_plugin()
            .call_handle_tool(&mut self.store, &wit_call)
        {
            Ok(Ok(res)) => Ok(Outcome {
                ok: res.ok,
                summary: res.summary,
                stdout: res.stdout,
                stderr: res.stderr,
                exit_code: res.exit_code,
                execution: res.execution.map(|execution| OutcomeExecution {
                    tool_name: execution.tool_name,
                    args: execution.args,
                    duration_ms: execution.duration_ms,
                    ok: execution.ok,
                    exit_code: execution.exit_code,
                    summary: execution.summary,
                }),
            }),
            Ok(Err(e)) => Err(plugin_err(e)),
            Err(e) => Err(anyhow::anyhow!("handle-tool 调用失败: {e}")),
        }
    }

    /// 关闭插件。
    pub fn shutdown(&mut self) -> Result<()> {
        match self
            .instance
            .tiangong_plugin_plugin()
            .call_shutdown(&mut self.store)
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(plugin_err(e)),
            Err(e) => Err(anyhow::anyhow!("shutdown 调用失败: {e}")),
        }
    }

    // ── 通用生命周期 ──

    /// 注入工作目录与信任模式（对应 `Plugin::set_workspace`）。
    pub fn set_workspace(&mut self, workspace: Option<String>, full_trust: bool) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_set_workspace(&mut self.store, workspace.as_deref(), full_trust)
            .map_err(|e| anyhow::anyhow!("set-workspace 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 通知 WASM 组件 CoreConfig 变更（对应 `Plugin::on_config_updated`）。
    /// `config_json` 为 CoreConfig 的 JSON 文本。
    pub fn on_config_updated(&mut self, config_json: String) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_on_config_updated(&mut self.store, &config_json)
            .map_err(|e| anyhow::anyhow!("on-config-updated 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 会话就绪钩子。
    pub fn on_session_ready(&mut self, session_json: String) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_on_session_ready(&mut self.store, &session_json)
            .map_err(|e| anyhow::anyhow!("on-session-ready 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 轮次开始钩子。
    pub fn on_turn_started(&mut self, session_json: String, turn_start_idx: u32) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_on_turn_started(&mut self.store, &session_json, turn_start_idx)
            .map_err(|e| anyhow::anyhow!("on-turn-started 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 轮次结束钩子（触发 micro 反刍）。
    pub fn on_turn_finished(&mut self, session_json: String, turn_start_idx: u32) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_on_turn_finished(&mut self.store, &session_json, turn_start_idx)
            .map_err(|e| anyhow::anyhow!("on-turn-finished 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    /// 会话结束钩子（触发 meso 反刍）。
    pub fn on_session_ended(&mut self, session_json: String) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_on_session_ended(&mut self.store, &session_json)
            .map_err(|e| anyhow::anyhow!("on-session-ended 调用失败: {e}"))?
            .map_err(plugin_err)
    }

    // ── UI 贡献：设置页 ──

    /// 插件贡献的设置页入口列表。
    pub fn contributions(&mut self) -> Result<Vec<Contribution>> {
        Ok(self
            .instance
            .tiangong_plugin_plugin_ui()
            .call_contributions(&mut self.store)
            .map_err(|e| anyhow::anyhow!("contributions 调用失败: {e}"))?
            .map_err(plugin_err)?
            .into_iter()
            .map(|c| Contribution {
                id: c.id,
                title: c.title,
                description: c.description,
                icon: c.icon,
                group: c.group,
                has_view: c.has_view,
            })
            .collect())
    }

    /// 打开页面，返回入口 HTML。
    pub fn open_view(&mut self, contribution_id: String) -> Result<String> {
        Ok(self
            .instance
            .tiangong_plugin_plugin_ui()
            .call_open_view(&mut self.store, &contribution_id)
            .map_err(|e| anyhow::anyhow!("open-view 调用失败: {e}"))?
            .map_err(plugin_err)?
            .html)
    }

    /// 获取页面资源（返回字节 + MIME）。
    pub fn get_view_resource(&mut self, path: String) -> Result<(Vec<u8>, String)> {
        let res = self
            .instance
            .tiangong_plugin_plugin_ui()
            .call_get_view_resource(&mut self.store, &path)
            .map_err(|e| anyhow::anyhow!("get-view-resource 调用失败: {e}"))?
            .map_err(plugin_err)?;
        Ok((res.data, res.mime))
    }

    /// 处理页面消息（iframe ↔ 插件双向通信）。
    pub fn handle_view_message(&mut self, method: String, payload: String) -> Result<String> {
        Ok(self
            .instance
            .tiangong_plugin_plugin_ui()
            .call_handle_view_message(
                &mut self.store,
                &crate::bindings::exports::tiangong::plugin::plugin_ui::ViewMessageRequest {
                    method,
                    payload,
                },
            )
            .map_err(|e| anyhow::anyhow!("handle-view-message 调用失败: {e}"))?
            .map_err(plugin_err)?
            .payload)
    }

    /// 引擎句柄（共享 Engine 的引用，测试与 epoch 心跳用）。
    pub fn engine(&self) -> &'static Engine {
        shared_engine()
    }

    /// 暴露 store 的可变借用，用于测试断言（如剩余 fuel）。
    pub fn store(&mut self) -> &mut Store<HostState> {
        &mut self.store
    }
}

/// 轻量工具调用入参，解耦宿主对 wasmtime 生成类型的依赖。
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 描述符结果。
#[derive(Debug, Clone)]
pub struct Descriptor {
    pub id: String,
    pub name: String,
    pub version: String,
}

/// 工具规格结果（JSON Schema 仍为文本）。
#[derive(Debug)]
pub struct Spec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// 工具调用结果。
#[derive(Debug)]
pub struct Outcome {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub execution: Option<OutcomeExecution>,
}

#[derive(Debug)]
pub struct OutcomeExecution {
    pub tool_name: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub ok: bool,
    pub exit_code: i32,
    pub summary: String,
}

/// 设置页贡献项（镜像 WIT record）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Contribution {
    pub id: String,
    pub title: String,
    pub description: String,
    pub icon: String,
    pub group: String,
    pub has_view: bool,
}

/// 把 WIT 层的 `plugin-error` 转为 anyhow。
fn plugin_err(e: PluginError) -> anyhow::Error {
    match e {
        PluginError::Message(m) => anyhow::anyhow!(m),
    }
}
