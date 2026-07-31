//! WASM 组件加载器。
//!
//! 负责：
//! - 创建开启 fuel + epoch 中断的 [`Engine`]；
//! - 读取并编译单文件 `.wasm` Component；
//! - 在资源受限的 [`Store`] 中实例化；
//! - 返回可被宿主当作 `Plugin` 使用的 [`WasmPlugin`]。
//!
//! 阶段一 PoC 不实现热加载、版本快照与权限探测；每个 `.wasm` 文件实例化一次。

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};

use crate::bindings::TiangongPlugin;
use crate::bindings::exports::tiangong::plugin::plugin::{PluginError, ToolCall as WitToolCall};
use crate::config::PluginRuntimeConfig;
use crate::host_state::HostState;
use crate::sidecar::SidecarConnection;

/// WASM 组件加载器。
///
/// 持有一个共享的 [`Engine`]（编译缓存复用）和一个用于实例化的 [`Linker`]。
/// 可选注入 [`SidecarConnection`]，供 sidecar host import 转发请求。
pub struct WasmPluginLoader {
    engine: Engine,
    linker: Arc<Linker<HostState>>,
    sidecar: Option<Arc<dyn SidecarConnection>>,
}

/// host import 的 host_getter：返回 HostState 自身的可变借用。
fn host_self_getter(state: &mut HostState) -> &mut HostState {
    state
}

impl WasmPluginLoader {
    /// 以给定配置创建加载器，不注入 sidecar（invoke 返回 unavailable）。
    pub fn new(config: &PluginRuntimeConfig) -> Result<Self> {
        Self::with_sidecar(config, None)
    }

    /// 以给定配置创建加载器，并注入 sidecar 连接。
    pub fn with_sidecar(
        _config: &PluginRuntimeConfig,
        sidecar: Option<Arc<dyn SidecarConnection>>,
    ) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        cfg.strategy(wasmtime::Strategy::Cranelift);
        let engine = Engine::new(&cfg).map_err(|e| anyhow::anyhow!("创建引擎失败: {e}"))?;

        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("接入 WASI 失败: {e}"))?;
        crate::bindings::tiangong::plugin::clock::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_self_getter,
        )
        .map_err(|e| anyhow::anyhow!("接入 clock 失败: {e}"))?;
        crate::bindings::tiangong::plugin::sidecar::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_self_getter,
        )
        .map_err(|e| anyhow::anyhow!("接入 sidecar 失败: {e}"))?;
        let linker = Arc::new(linker);

        Ok(Self {
            engine,
            linker,
            sidecar,
        })
    }

    /// 引擎句柄，供外部（如 epoch 心跳线程）调用 `increment_epoch`。
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// 加载并实例化一个 `.wasm` Component，返回宿主侧 [`WasmPlugin`]。
    pub fn load(&self, wasm_path: &Path, config: &PluginRuntimeConfig) -> Result<WasmPlugin> {
        let bytes = std::fs::read(wasm_path).map_err(|e| {
            anyhow::anyhow!("读取 wasm 组件失败 {path}: {e}", path = wasm_path.display())
        })?;
        let component = Component::new(&self.engine, bytes).map_err(|e| {
            anyhow::anyhow!("编译 wasm 组件失败 {path}: {e}", path = wasm_path.display())
        })?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(config.memory_limit)
            .build();

        let mut store = Store::new(
            &self.engine,
            HostState::new(limits, self.sidecar.clone(), "memory".to_string()),
        );
        // 注册内存/表/实例上限：limiter 闭包返回 StoreLimits 的借用。
        store.limiter(|state: &mut HostState| state.limits_mut());
        // fuel 在每次工具调用前重置；实例化阶段也给足 fuel。
        // set_fuel 仅在未开启 consume_fuel 时返回 Err，此处配置已开启，故安全忽略。
        let _ = store.set_fuel(config.fuel_limit);
        // epoch：实例化阶段给一个宽裕的 deadline，避免初始化被误中断。
        // 工具调用时再按 config.epoch_deadline_ticks() 重置为实际限制。
        store.set_epoch_deadline(u64::MAX);

        let instance = TiangongPlugin::instantiate(&mut store, &component, &self.linker)
            .map_err(|e| anyhow::anyhow!("实例化 wasm 组件失败: {e}"))?;

        Ok(WasmPlugin {
            engine: self.engine.clone(),
            #[allow(unused_variables)]
            linker: self.linker.clone(),
            instance,
            store,
        })
    }
}

/// 宿主侧持有的已实例化 WASM 插件句柄。
///
/// 内部包含独立的 [`Store`]，每次工具调用在调用前重置 fuel 与 epoch deadline，
/// 保证单次调用可被独立限制与终止。
pub struct WasmPlugin {
    engine: Engine,
    #[allow(dead_code)]
    linker: Arc<Linker<HostState>>,
    instance: TiangongPlugin,
    store: Store<HostState>,
}

impl WasmPlugin {
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

    /// 注入工作目录（对应 `Plugin::set_workspace`）。
    pub fn set_workspace(&mut self, workspace: Option<String>) -> Result<()> {
        self.instance
            .tiangong_plugin_plugin()
            .call_set_workspace(&mut self.store, workspace.as_deref())
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

    /// 引擎句柄（测试与 epoch 心跳用）。
    pub fn engine(&self) -> &Engine {
        &self.engine
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
#[derive(Debug)]
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
