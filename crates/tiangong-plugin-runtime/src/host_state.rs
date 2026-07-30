//! Store 内部的宿主状态。
//!
//! 阶段一 PoC 不向 WASM 组件提供业务 host import，但 WASI Preview 2 组件
//! 默认依赖一组标准 WASI 接口（如 `wasi:io/poll`），因此状态需承载一个
//! [`WasiCtx`] 与资源表，并实现 [`WasiView`]。
//! 后续阶段接入 storage / model 等 host 能力时，在此扩展。

use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// WASM Store 的宿主侧状态。
pub struct HostState {
    limits: StoreLimits,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl HostState {
    pub fn new(limits: StoreLimits) -> Self {
        // 阶段一 PoC：使用最小 WASI 上下文（无 stdio / 无 fs / 无 socket），
        // 仅满足组件对 poll 等基础接口的导入依赖。
        let wasi = WasiCtxBuilder::new().build();
        Self {
            limits,
            wasi,
            table: ResourceTable::new(),
        }
    }

    /// 提供对内部限制器的可变借用，供 `Store::limiter` 闭包返回。
    pub fn limits_mut(&mut self) -> &mut StoreLimits {
        &mut self.limits
    }
}

/// 让 wasmtime-wasi 经由该状态访问 WASI 上下文与资源表。
impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
