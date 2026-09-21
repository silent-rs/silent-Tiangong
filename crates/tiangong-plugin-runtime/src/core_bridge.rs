//! Core 的 runtime 聚合桥：Core 插件列表的唯一编译期成员。
//!
//! 桌面端 Core 的 `plugins` 列表只持有一个 [`RuntimeCorePlugin`]——构造后
//! 永不变化，对 Core 而言插件集合是稳定的。已安装插件（WASM/TS 工具）的
//! 全部能力（工具声明、执行路由、prompt 段落、@提及、生命周期钩子、
//! exec_env）由桥在**被调用时**向 runtime 注册表聚合：
//!
//! - 新装插件：下一次聚合自然出现（无需通知 Core，turn 进行中不重聚，
//!   下一轮生效）；
//! - 启停/升级/卸载：runtime 经 Weak 就地更新既有适配器，桥不感知；
//! - 差量装载的三段式锁纪律见 [`RuntimeCorePlugin::adapters`]。
//!
//! 每个桥实例持独立的交付表（per-Core 适配器隔离，与静态装配时代
//! 「各 Core 独立实例化」语义一致）。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tiangong_core::core::plugin::Plugin;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::Session;
use tiangong_core::tools::extension::{
    MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
};
use tiangong_core::tools::result::ToolResult;
use tiangong_llm::tool::{ToolCall, ToolSpec};

use crate::registry::{self, RuntimeKind};

/// Core 侧的 runtime 聚合插件。
pub struct RuntimeCorePlugin {
    storage_root: PathBuf,
    runtime: RuntimeKind,
    /// 已交付适配器（plugin_id → 适配器），per-Core 隔离。
    delivered: Mutex<HashMap<String, Arc<dyn Plugin>>>,
    /// 最近一次聚合构建的工具路由表（tool_name → 拥有者适配器）。
    /// `tool_specs` 聚合时重建；`handle` 只读查询。
    tool_routes: RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl RuntimeCorePlugin {
    /// 构造桌面端桥实例。
    pub fn desktop(storage_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            storage_root,
            runtime: RuntimeKind::Desktop,
            delivered: Mutex::new(HashMap::new()),
            tool_routes: RwLock::new(HashMap::new()),
        })
    }

    /// 当前应交付的适配器集合（三段式差量同步）。
    ///
    /// **锁纪律**：`delivered` 锁的两段持有都只做纯内存操作（微秒级）；
    /// 慢操作（`load_core_plugin` 的 WASM 实例化、与迁移路径的注册表锁
    /// 竞争）全部在**锁外**执行——否则并发聚合（`tool_specs` 与
    /// `mention_candidates` 同时到达）会在本锁上串行卡等。
    /// 并发首见同一新插件时可能各自装载一次，合并段只保留先插入者；
    /// 落选适配器随 Weak 失效被回收，无泄漏。
    fn adapters(&self) -> Vec<Arc<dyn Plugin>> {
        let ids = registry::core_plugin_ids(&self.storage_root, self.runtime);
        let missing = {
            let Ok(mut delivered) = self.delivered.lock() else {
                return Vec::new();
            };
            delivered.retain(|id, _| ids.contains(id));
            ids.iter()
                .filter(|id| !delivered.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut loaded = Vec::with_capacity(missing.len());
        for id in missing {
            if let Some(adapter) = registry::load_core_plugin(&id, self.runtime) {
                loaded.push((id, adapter));
            }
        }
        let Ok(mut delivered) = self.delivered.lock() else {
            return Vec::new();
        };
        for (id, adapter) in loaded {
            delivered.entry(id).or_insert(adapter);
        }
        // core_plugin_ids 已按（prompt 置顶 + id 字典序）排序，
        // 与 core::plugin::prepare_plugins 的插件序一致。
        delivered.values().cloned().collect()
    }

    /// 用聚合结果重建工具路由表，返回聚合的工具声明（含去重）。
    fn aggregate_tool_specs(&self) -> Vec<ToolSpec> {
        let adapters = self.adapters();
        let mut specs = Vec::new();
        let mut routes = HashMap::new();
        for adapter in &adapters {
            for spec in adapter.tool_specs() {
                if !routes.contains_key(&spec.name) {
                    routes.insert(spec.name.clone(), adapter.clone());
                    specs.push(spec);
                }
            }
        }
        if let Ok(mut tool_routes) = self.tool_routes.write() {
            *tool_routes = routes;
        }
        specs
    }

    fn each_adapter(&self, mut apply: impl FnMut(&Arc<dyn Plugin>)) {
        for adapter in self.adapters() {
            apply(&adapter);
        }
    }
}

impl Plugin for RuntimeCorePlugin {
    fn id(&self) -> &str {
        "plugin-runtime"
    }

    fn set_execution_context(&self, workspace: Option<&std::path::Path>, trust: TrustMode) {
        self.each_adapter(|adapter| adapter.set_execution_context(workspace, trust));
    }

    fn set_feedback_tx(&self, tx: tiangong_core::core::plugin::PluginFeedbackTx) {
        self.each_adapter(|adapter| adapter.set_feedback_tx(tx.clone()));
    }

    fn exec_env(&self) -> std::collections::BTreeMap<String, String> {
        let mut merged = std::collections::BTreeMap::new();
        for adapter in self.adapters() {
            for (key, value) in adapter.exec_env() {
                merged.insert(key, value);
            }
        }
        merged
    }

    fn set_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        self.each_adapter(|adapter| adapter.set_exec_env(env.clone()));
    }

    fn on_cancel<'a>(
        &'a self,
        session: &mut Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        // 与各适配器实现约定一致：取消工作在同步段完成，返回的 future 为
        // 空壳（见 WasmPluginAdapter::on_cancel）。桥在同步段逐适配器驱动，
        // session 引用不进入返回的 future——这正是 trait 生命周期签名的
        // 隐含约定；若未来出现真正的异步取消实现，需扩展 trait 签名。
        let adapters = self.adapters();
        for adapter in adapters {
            let future = adapter.on_cancel(session);
            let mut future = std::pin::pin!(future);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            let _ = future.as_mut().poll(&mut context);
        }
        Box::pin(async {})
    }

    fn on_config_updated(&self, config: &tiangong_core::config::core::CoreConfig) {
        self.each_adapter(|adapter| adapter.on_config_updated(config));
    }

    fn on_session_ready(&self, session: &mut Session) {
        self.each_adapter(|adapter| adapter.on_session_ready(session));
    }

    fn on_turn_started(&self, session: &mut Session, turn_start_idx: usize) {
        self.each_adapter(|adapter| adapter.on_turn_started(session, turn_start_idx));
    }

    fn on_turn_finished(&self, session: &Session, turn_start_idx: usize) {
        self.each_adapter(|adapter| adapter.on_turn_finished(session, turn_start_idx));
    }

    fn on_session_ended(&self, session: &Session) {
        self.each_adapter(|adapter| adapter.on_session_ended(session));
    }
}

impl ToolSpecProvider for RuntimeCorePlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.aggregate_tool_specs()
    }
}

impl ToolOverrideHandler for RuntimeCorePlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let owner = self
            .tool_routes
            .read()
            .ok()
            .and_then(|routes| routes.get(&call.name).cloned());
        match owner {
            Some(adapter) => adapter.handle(call, session, actor_id),
            // 路由表在 tool_specs 聚合时重建；未知工具名不拦截，
            // 交回 core 默认逻辑。
            None => Box::pin(async { None }),
        }
    }
}

impl PromptSectionProvider for RuntimeCorePlugin {
    fn prompt_sections(&self) -> Vec<String> {
        let mut sections = Vec::new();
        for adapter in self.adapters() {
            sections.extend(adapter.prompt_sections());
        }
        sections
    }
}

impl MentionCandidateProvider for RuntimeCorePlugin {
    fn mention_candidates(&self) -> Vec<tiangong_core::MentionCandidate> {
        let mut candidates = Vec::new();
        for adapter in self.adapters() {
            candidates.extend(adapter.mention_candidates());
        }
        candidates
    }
}
