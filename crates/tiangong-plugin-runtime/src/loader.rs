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
use crate::bindings::exports::tiangong::plugin::plugin::{
    MemoryKind as WitMemoryKind, PluginError, RecallHit as WitRecallHit,
    SearchStrategy as WitSearchStrategy, ToolCall as WitToolCall,
};
use crate::config::PluginRuntimeConfig;
use crate::host_state::HostState;

/// WASM 组件加载器。
///
/// 持有一个共享的 [`Engine`]（编译缓存复用）和一个用于实例化的 [`Linker`]。
/// 阶段一 PoC 不提供任何 host import；后续阶段在此注入 storage / model 等 host 能力。
pub struct WasmPluginLoader {
    engine: Engine,
    linker: Arc<Linker<HostState>>,
}

/// clock host import 的 host_getter：返回 HostState 自身的可变借用。
fn host_clock_getter(state: &mut HostState) -> &mut HostState {
    state
}

impl WasmPluginLoader {
    /// 以给定配置创建加载器。
    pub fn new(_config: &PluginRuntimeConfig) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        // cranelift 是默认后端，显式指定以保证优化。
        cfg.strategy(wasmtime::Strategy::Cranelift);
        let engine = Engine::new(&cfg).map_err(|e| anyhow::anyhow!("创建引擎失败: {e}"))?;

        // 接入 WASI Preview 2：WASIp2 组件默认导入 poll 等基础接口，
        // 需在 Linker 中提供实现（绑定到 HostState 的 WasiView）。
        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| anyhow::anyhow!("接入 WASI 失败: {e}"))?;
        // 接入 clock host import：HostState 已实现 clock::Host，经 HasSelf 暴露自身。
        crate::bindings::tiangong::plugin::clock::add_to_linker::<HostState, HasSelf<HostState>>(
            &mut linker,
            host_clock_getter,
        )
        .map_err(|e| anyhow::anyhow!("接入 clock 失败: {e}"))?;
        let linker = Arc::new(linker);

        Ok(Self { engine, linker })
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

        let mut store = Store::new(&self.engine, HostState::new(limits));
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

    // ── 阶段二：下沉的纯逻辑导出 ──

    /// 融合两路召回（BM25 + 语义），返回 topK。
    pub fn rerank_fuse(
        &mut self,
        bm25: Vec<FusedHit>,
        semantic: Vec<FusedHit>,
        semantic_ratio: f64,
        limit: u32,
    ) -> Result<Vec<FusedHit>> {
        let wit_bm25: Vec<_> = bm25.into_iter().map(Into::into).collect();
        let wit_sem: Vec<_> = semantic.into_iter().map(Into::into).collect();
        let res = self
            .instance
            .tiangong_plugin_plugin()
            .call_rerank_fuse(&mut self.store, &wit_bm25, &wit_sem, semantic_ratio, limit)
            .map_err(|e| anyhow::anyhow!("rerank-fuse 调用失败: {e}"))?
            .map_err(plugin_err)?;
        Ok(res.into_iter().map(Into::into).collect())
    }

    /// 规则规划检索锚点（对应 WASM 内 fallback_plan）。
    pub fn plan_recall_fallback(
        &mut self,
        query: String,
        reason: Option<String>,
        expected: Vec<String>,
        context: Vec<String>,
        limit: u32,
    ) -> Result<PlannedRecall> {
        let reason_ref = reason.as_deref();
        let res = self
            .instance
            .tiangong_plugin_plugin()
            .call_plan_recall_fallback(
                &mut self.store,
                &query,
                reason_ref,
                &expected,
                &context,
                limit,
            )
            .map_err(|e| anyhow::anyhow!("plan-recall-fallback 调用失败: {e}"))?
            .map_err(plugin_err)?;
        Ok(PlannedRecall {
            query: res.anchors.query,
            keywords: res.anchors.keywords,
            strategy: res.anchors.strategy.map(Into::into),
            limit: res.limit,
            used_llm: res.used_llm,
        })
    }

    /// 规则整理召回结果为文本（对应 WASM 内 fallback_synthesis）。
    pub fn synthesize_fallback(
        &mut self,
        query: String,
        context: Vec<String>,
        hits: Vec<FusedHit>,
    ) -> Result<String> {
        let wit_hits: Vec<_> = hits.into_iter().map(Into::into).collect();
        self.instance
            .tiangong_plugin_plugin()
            .call_synthesize_fallback(&mut self.store, &query, &context, &wit_hits)
            .map_err(|e| anyhow::anyhow!("synthesize-fallback 调用失败: {e}"))?
            .map_err(plugin_err)
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

/// 把 WIT 层的 `plugin-error` 转为 anyhow。
fn plugin_err(e: PluginError) -> anyhow::Error {
    match e {
        PluginError::Message(m) => anyhow::anyhow!(m),
    }
}

// ── 阶段二：下沉逻辑的 host 侧轻量类型 ──

/// 记忆类型（镜像 WIT enum）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryKind {
    Episode,
    Entity,
    Decision,
    Evidence,
}

/// 检索策略（镜像 WIT variant）。
#[derive(Debug, Clone, PartialEq)]
pub enum SearchStrategy {
    Skip,
    Keyword,
    Semantic,
    Hybrid(f64),
}

/// 融合/召回命中项（镜像 WIT record）。
#[derive(Debug, Clone)]
pub struct FusedHit {
    pub node_id: String,
    pub title: String,
    pub summary: String,
    pub score: f64,
    pub kind: MemoryKind,
    pub importance: f64,
    pub depth1_loaded: bool,
}

/// 规则规划产出（镜像 WASM 的 planned-recall）。
#[derive(Debug, Clone)]
pub struct PlannedRecall {
    pub query: String,
    pub keywords: Vec<String>,
    pub strategy: Option<SearchStrategy>,
    pub limit: u32,
    pub used_llm: bool,
}

impl From<FusedHit> for WitRecallHit {
    fn from(h: FusedHit) -> Self {
        Self {
            node_id: h.node_id,
            title: h.title,
            summary: h.summary,
            score: h.score,
            kind: h.kind.into(),
            importance: h.importance,
            depth1_loaded: h.depth1_loaded,
        }
    }
}

impl From<WitRecallHit> for FusedHit {
    fn from(h: WitRecallHit) -> Self {
        Self {
            node_id: h.node_id,
            title: h.title,
            summary: h.summary,
            score: h.score,
            kind: h.kind.into(),
            importance: h.importance,
            depth1_loaded: h.depth1_loaded,
        }
    }
}

impl From<MemoryKind> for WitMemoryKind {
    fn from(k: MemoryKind) -> Self {
        match k {
            MemoryKind::Episode => Self::Episode,
            MemoryKind::Entity => Self::Entity,
            MemoryKind::Decision => Self::Decision,
            MemoryKind::Evidence => Self::Evidence,
        }
    }
}

impl From<WitMemoryKind> for MemoryKind {
    fn from(k: WitMemoryKind) -> Self {
        match k {
            WitMemoryKind::Episode => Self::Episode,
            WitMemoryKind::Entity => Self::Entity,
            WitMemoryKind::Decision => Self::Decision,
            WitMemoryKind::Evidence => Self::Evidence,
        }
    }
}

impl From<SearchStrategy> for WitSearchStrategy {
    fn from(s: SearchStrategy) -> Self {
        match s {
            SearchStrategy::Skip => Self::Skip,
            SearchStrategy::Keyword => Self::Keyword,
            SearchStrategy::Semantic => Self::Semantic,
            SearchStrategy::Hybrid(r) => Self::Hybrid(r),
        }
    }
}

impl From<WitSearchStrategy> for SearchStrategy {
    fn from(s: WitSearchStrategy) -> Self {
        match s {
            WitSearchStrategy::Skip => Self::Skip,
            WitSearchStrategy::Keyword => Self::Keyword,
            WitSearchStrategy::Semantic => Self::Semantic,
            WitSearchStrategy::Hybrid(r) => Self::Hybrid(r),
        }
    }
}
