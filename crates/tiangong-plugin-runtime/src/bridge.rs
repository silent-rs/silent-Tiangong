//! 宿主桥接（Host Bridge）：插件 UI 与宿主之间的统一通信管道。
//!
//! 插件 UI 沙箱内经 `bridge.call(method, payload)` 调用宿主能力、经
//! `bridge.on(channel)` 订阅宿主事件。本模块只做命名空间白名单、权限校验
//! 与负载透传，不解析业务 JSON、不感知具体插件（宿主中性）。
//!
//! M0 范围：`plugin.*` 转发到本插件 WASM 的 `handle-view-message`（等价旧
//! `plugin_call` 通道）；其余命名空间完成白名单与权限定形，宿主服务路由在
//! 对应接缝任务中接入。事件订阅当前只提供登记骨架，事件源在事件接缝任务接入。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, bail};

use crate::manifest::PluginManifest;

/// 一个桥接能力命名空间：方法前缀 → 所需权限。
#[derive(Debug, Clone)]
pub struct BridgeNamespace {
    /// 方法前缀，如 `plugin.`。
    pub prefix: &'static str,
    /// 调用该命名空间所需的权限声明。
    pub permission: &'static str,
    /// 说明（供错误信息与文档）。
    pub description: &'static str,
}

/// 首版桥接命名空间白名单（设计文档 6.3）。
pub const BRIDGE_NAMESPACES: &[BridgeNamespace] = &[
    BridgeNamespace {
        prefix: "plugin.",
        permission: "bridge.call",
        description: "转发到本插件 WASM 逻辑层",
    },
    BridgeNamespace {
        prefix: "session.",
        permission: "session.read",
        description: "读取/操作当前会话",
    },
    BridgeNamespace {
        prefix: "tool.",
        permission: "tool.read",
        description: "主动触发工具、读取工具执行结果",
    },
    BridgeNamespace {
        prefix: "approval.",
        permission: "approval.handle",
        description: "响应审批、查询审批状态",
    },
    BridgeNamespace {
        prefix: "interaction.",
        permission: "interaction.handle",
        description: "发起/响应交互请求",
    },
    BridgeNamespace {
        prefix: "storage.",
        permission: "storage.private",
        description: "读写插件私有数据",
    },
];

/// 事件订阅的合法命名空间前缀（设计文档 7.7）。
pub const EVENT_NAMESPACE_PREFIXES: &[&str] =
    &["session.", "tool.", "approval.", "lifecycle.", "config."];

/// 查找 method 所属的桥接命名空间。
pub fn namespace_of(method: &str) -> Option<&'static BridgeNamespace> {
    BRIDGE_NAMESPACES
        .iter()
        .find(|namespace| method.starts_with(namespace.prefix))
}

/// 权限校验。
///
/// v1 清单 `permissions` 为空时放行（等价旧 `plugin_call` 无权限校验的行为，
/// 保证现有 WASM 插件零改动）；v2 与显式声明过权限的 v1 按声明校验。
fn has_bridge_permission(manifest: &PluginManifest, permission: &str) -> bool {
    if manifest.schema_version == 1 && manifest.permissions.is_empty() {
        return true;
    }
    manifest.has_permission(permission)
}

/// 判断事件订阅声明（`capabilities.events`）是否覆盖某 channel。
///
/// 支持精确匹配、`<ns>.*` 通配与 `*` 全量。
pub fn event_declaration_allows(declared: &[String], channel: &str) -> bool {
    declared.iter().any(|pattern| {
        pattern == channel
            || pattern == "*"
            || pattern
                .strip_suffix(".*")
                .is_some_and(|prefix| channel.starts_with(&format!("{prefix}.")))
    })
}

// ── bridge.call ──

/// 处理一次 `bridge.call`：鉴权 → 白名单 → 路由 → 返回序列化结果。
///
/// `payload` 为不透明负载，透传不解析。
pub fn bridge_call(plugin_id: &str, method: &str, payload: &str) -> Result<String> {
    let Some(namespace) = namespace_of(method) else {
        tracing::warn!(plugin_id, method, "bridge.call 拒绝未知 method");
        bail!(
            "bridge.call 拒绝未知 method {method}（插件 {plugin_id}），可命名空间：{}",
            BRIDGE_NAMESPACES
                .iter()
                .map(|ns| ns.prefix)
                .collect::<Vec<_>>()
                .join(" ")
        );
    };

    let manifest = crate::registry::plugin_manifest(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("bridge.call 插件 {plugin_id} 未加载"))?;
    if !has_bridge_permission(&manifest, namespace.permission) {
        tracing::warn!(
            plugin_id,
            method,
            permission = namespace.permission,
            "bridge.call 权限不足"
        );
        bail!(
            "bridge.call 插件 {plugin_id} 未声明权限 {}，无法调用 {}",
            namespace.permission,
            namespace.prefix
        );
    }

    match namespace.prefix {
        // 转发到本插件 WASM 逻辑层：剥掉 `plugin.` 前缀，剩余部分即 WASM 方法名，
        // 负载透传（等价旧 plugin_call 通道）。
        "plugin." => {
            let wasm_method = &method[namespace.prefix.len()..];
            if wasm_method.is_empty() {
                bail!("bridge.call method {method} 缺少插件方法名");
            }
            crate::registry::handle_view_message(plugin_id, wasm_method, payload)
                .ok_or_else(|| anyhow::anyhow!("bridge.call 插件 {plugin_id} 处理消息失败"))
        }
        // 其余命名空间已定形白名单，宿主服务路由在对应接缝任务接入。
        _ => {
            tracing::info!(plugin_id, method, "bridge.call 命名空间尚未接入宿主服务");
            bail!(
                "bridge.call 命名空间 {} 已登记但当前版本尚未接入宿主服务",
                namespace.prefix
            )
        }
    }
}

// ── bridge.on / bridge.off ──

/// 事件推送回调：`(plugin_id, channel, payload)`。
pub type BridgeEventEmitter = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;

static EVENT_EMITTER: OnceLock<BridgeEventEmitter> = OnceLock::new();
static EVENT_SUBSCRIPTIONS: OnceLock<Mutex<BTreeMap<String, Vec<String>>>> = OnceLock::new();

fn event_subscriptions() -> &'static Mutex<BTreeMap<String, Vec<String>>> {
    EVENT_SUBSCRIPTIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 注入事件推送回调（宿主入口启动时调用，把订阅事件送达前端/插件 UI）。
pub fn set_event_emitter(emitter: BridgeEventEmitter) {
    let _ = EVENT_EMITTER.set(emitter);
}

/// 订阅宿主事件通道。
///
/// channel 必须属于 [`EVENT_NAMESPACE_PREFIXES`] 登记的命名空间，且插件在
/// `capabilities.events` 中声明过对应命名空间（最小授权）。
pub fn bridge_subscribe(plugin_id: &str, channel: &str) -> Result<()> {
    if !EVENT_NAMESPACE_PREFIXES
        .iter()
        .any(|prefix| channel.starts_with(prefix))
    {
        tracing::warn!(plugin_id, channel, "bridge.on 拒绝未知 channel");
        bail!(
            "bridge.on 拒绝未知 channel {channel}（插件 {plugin_id}），可命名空间：{}",
            EVENT_NAMESPACE_PREFIXES.join(" ")
        );
    }
    let manifest = crate::registry::plugin_manifest(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("bridge.on 插件 {plugin_id} 未加载"))?;
    let declared = manifest
        .capabilities
        .as_ref()
        .map(|capabilities| capabilities.events.as_slice())
        .unwrap_or_default();
    if !event_declaration_allows(declared, channel) {
        tracing::warn!(
            plugin_id,
            channel,
            "bridge.on 超出 capabilities.events 授权"
        );
        bail!("bridge.on 插件 {plugin_id} 未在 capabilities.events 声明覆盖 {channel} 的命名空间");
    }

    let mut subscriptions = event_subscriptions()
        .lock()
        .map_err(|_| anyhow::anyhow!("事件订阅表已损坏"))?;
    let channels = subscriptions.entry(plugin_id.to_string()).or_default();
    if !channels.iter().any(|item| item == channel) {
        channels.push(channel.to_string());
    }
    Ok(())
}

/// 取消订阅（插件 UI 卸载时调用）。
pub fn bridge_unsubscribe(plugin_id: &str, channel: &str) -> Result<()> {
    let mut subscriptions = event_subscriptions()
        .lock()
        .map_err(|_| anyhow::anyhow!("事件订阅表已损坏"))?;
    if let Some(channels) = subscriptions.get_mut(plugin_id) {
        channels.retain(|item| item != channel);
        if channels.is_empty() {
            subscriptions.remove(plugin_id);
        }
    }
    Ok(())
}

/// 卸载插件时清空其全部订阅（贡献可逆原则）。
pub fn clear_plugin_subscriptions(plugin_id: &str) {
    if let Ok(mut subscriptions) = event_subscriptions().lock() {
        subscriptions.remove(plugin_id);
    }
}

/// 宿主事件源向已订阅插件推送事件。
///
/// `channel` 为具体事件名（如 `session.updated`），`payload` 为不透明负载。
/// 事件源在事件接缝任务接入，本接口先行定形。
pub fn bridge_emit(channel: &str, payload: &str) {
    let Some(emitter) = EVENT_EMITTER.get() else {
        return;
    };
    let Ok(subscriptions) = event_subscriptions().lock() else {
        return;
    };
    for (plugin_id, channels) in subscriptions.iter() {
        if channels.iter().any(|item| item == channel) {
            emitter(plugin_id, channel, payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 命名空间识别正确() {
        assert_eq!(namespace_of("plugin.getConfig").unwrap().prefix, "plugin.");
        assert_eq!(
            namespace_of("session.getMessages").unwrap().prefix,
            "session."
        );
        assert_eq!(namespace_of("storage.read").unwrap().prefix, "storage.");
        assert!(namespace_of("rag.query").is_none());
        assert!(namespace_of("unknown").is_none());
    }

    #[test]
    fn 未知_method_错误信息可读() {
        let error = bridge_call("missing", "rag.query", "{}").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("拒绝未知 method"));
        assert!(message.contains("rag.query"));
        assert!(message.contains("plugin."));
    }

    #[test]
    fn 事件声明匹配规则() {
        let declared = vec![
            "session.*".to_string(),
            "tool.executed".to_string(),
            "*".to_string(),
        ];
        // 上面同时声明了通配与精确，覆盖判断应任一命中
        assert!(event_declaration_allows(&declared, "session.updated"));
        assert!(event_declaration_allows(&declared, "session.a.b"));
        assert!(event_declaration_allows(&declared, "tool.executed"));
        assert!(event_declaration_allows(&declared, "anything.else"));

        let declared = vec!["session.*".to_string()];
        assert!(event_declaration_allows(&declared, "session.updated"));
        assert!(!event_declaration_allows(&declared, "tool.started"));
        assert!(!event_declaration_allows(&declared, "sessions.updated"));

        let declared = vec!["tool.executed".to_string()];
        assert!(event_declaration_allows(&declared, "tool.executed"));
        assert!(!event_declaration_allows(&declared, "tool.started"));

        assert!(!event_declaration_allows(&[], "session.updated"));
    }

    #[test]
    fn 未知_channel_拒绝() {
        let error = bridge_subscribe("missing", "rag.updated").unwrap_err();
        assert!(format!("{error:#}").contains("拒绝未知 channel"));
        assert!(format!("{error:#}").contains("session."));
    }

    #[test]
    fn 权限校验规则覆盖_v1_v2() {
        use crate::manifest::PluginManifest;

        // v1 + 空 permissions：放行（等价旧 plugin_call 行为）
        let v1_empty: PluginManifest = serde_json::from_str(
            r#"{"schema_version":1,"id":"a","version":"1.0.0","wasm":{"binary":"p.wasm"},"permissions":[]}"#,
        )
        .unwrap();
        assert!(has_bridge_permission(&v1_empty, "bridge.call"));
        assert!(has_bridge_permission(&v1_empty, "session.read"));

        // v1 + 显式声明：按声明校验
        let v1_declared: PluginManifest = serde_json::from_str(
            r#"{"schema_version":1,"id":"a","version":"1.0.0","wasm":{"binary":"p.wasm"},"permissions":["bridge.call"]}"#,
        )
        .unwrap();
        assert!(has_bridge_permission(&v1_declared, "bridge.call"));
        assert!(!has_bridge_permission(&v1_declared, "session.read"));

        // v2：一律按声明校验
        let v2: PluginManifest = serde_json::from_str(
            r#"{"schema_version":2,"id":"a","version":"1.0.0","wasm":{"binary":"p.wasm"},"permissions":[]}"#,
        )
        .unwrap();
        assert!(!has_bridge_permission(&v2, "bridge.call"));
    }
}
