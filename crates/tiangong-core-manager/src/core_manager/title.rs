//! 会话标题自动生成（自 core 迁入，lite 客户端机制随迁）。
//!
//! 投递用户消息且标题仍是默认值时，后台用 lite 端点（未配置时回退 chat）
//! 据用户消息生成标题，经 [`TiangongCore::set_title`] 的 only_if_default
//! 语义写回：turn 进行中经命令通道排队、空闲时直接落盘，两种路径都不会
//! 覆盖用户已手动修改的标题。

use tiangong_core::core::is_default_title;
use tiangong_llm::SingleProviderClient;

use super::CoreManager;

impl CoreManager {
    /// 投递用户消息后按需触发标题生成（fire-and-forget，失败静默）。
    ///
    /// 预检标题仍是默认值后才调模型，避免每次发消息都白花一次请求；
    /// 写回侧 `set_title(only_if_default=true)` 会再校验一次，预检与写回
    /// 之间用户手动改名不会丢失。生成时会话 Core 可能已回收，届时放弃
    /// 本次结果——下次发消息发现标题仍是默认值会再次触发，天然自愈。
    pub(crate) fn spawn_title_generation_if_needed(&self, session_id: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let Ok(session) = self.load_session(session_id) else {
            return;
        };
        if !is_default_title(&session.title) {
            return;
        }
        // lite 端点不经 CoreConfig（core 只携带 chat），从磁盘模型配置的
        // Lite 路由槽解析；未配置时回退 chat。标题生成低频，实时读盘可接受
        // 且能感知模型配置热更。
        let endpoint = tiangong_config::io::load_models_config_at(&self.storage_root)
            .resolve_slot(tiangong_llm::models_config::RoutingSlot::Lite)
            .map(tiangong_llm::ModelEndpoint::from_resolved)
            .unwrap_or_else(|| self.config.snapshot().llm.clone());
        let manager = self.clone();
        let sid = session_id.to_string();
        let input = text.to_string();
        tokio::spawn(async move {
            let sid_for_client = sid.clone();
            let title = tokio::task::spawn_blocking(move || {
                // 绑定会话 id：自定义 header 的 ${session_id} 模板与缓存键依赖它
                //（与迁移前 ctx.lite_client().with_session_id 行为对齐）。
                SingleProviderClient::new(endpoint)
                    .with_session_id(sid_for_client)
                    .complete_lite(&input)
            })
            .await
            .ok()
            .and_then(|result| result.ok());
            let Some(title) = title else {
                return;
            };
            let clean = title.trim().trim_matches('"').to_string();
            if clean.is_empty() {
                return;
            }
            let registry = manager.registry();
            if let Some(core) = registry.get(&sid) {
                let _ = core.set_title(clean, true);
            }
        });
    }
}
