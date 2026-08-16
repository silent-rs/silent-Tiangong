//! 收尾钩子的统一通知投递（issue #404）。
//!
//! `on_turn_finished` / `on_session_ended` 是通知型钩子：Core 只保证通知最终
//! 送达插件，不等待完成、不收集结果、不重试——收尾成败与产出由插件实现自行
//! 负责（失败/超时自行记录日志，重活交给插件自身的 sidecar 或后台任务）。
//!
//! 投递为纯 fire-and-forget：每个通知一个短命后台线程（与 WASM 适配器旧
//! detached 模式同款，`std::thread` 无 runtime 依赖，在同步 spawn_blocking
//! 上下文中也安全），调用方立即返回。插件 panic 由 `catch_unwind` 兜底，
//! 仅记告警不影响后续通知。

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::thread;

use super::Plugin;
use crate::session::Session;

/// 后台投递 `on_turn_finished`：turn 终态已发布，通知即返回。
pub(crate) fn notify_turn_finished(
    plugins: &[Arc<dyn Plugin>],
    session: &Session,
    turn_start_idx: usize,
) {
    for plugin in plugins {
        let plugin = Arc::clone(plugin);
        let session = Arc::new(session.clone());
        let plugin_id = plugin.id().to_owned();
        let spawn_fail_id = plugin_id.clone();
        let spawned = thread::Builder::new()
            .name(format!("plugin-turn-finish-{plugin_id}"))
            .spawn(move || {
                if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    plugin.on_turn_finished(&session, turn_start_idx);
                })) {
                    tracing::warn!(
                        plugin_id,
                        ?panic,
                        "插件 on_turn_finished 通知 panic，已忽略"
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(plugin_id = %spawn_fail_id, %error, "插件 on_turn_finished 通知线程启动失败，通知丢弃");
        }
    }
}

/// 后台投递 `on_session_ended`：会话关闭立即返回，插件收尾自行收敛。
pub(crate) fn notify_session_ended(plugins: &[Arc<dyn Plugin>], session: &Session) {
    for plugin in plugins {
        let plugin = Arc::clone(plugin);
        let session = Arc::new(session.clone());
        let plugin_id = plugin.id().to_owned();
        let spawn_fail_id = plugin_id.clone();
        let spawned = thread::Builder::new()
            .name(format!("plugin-session-end-{plugin_id}"))
            .spawn(move || {
                if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    plugin.on_session_ended(&session);
                })) {
                    tracing::warn!(
                        plugin_id,
                        ?panic,
                        "插件 on_session_ended 通知 panic，已忽略"
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(plugin_id = %spawn_fail_id, %error, "插件 on_session_ended 通知线程启动失败，通知丢弃");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::super::Plugin;
    use crate::session::Session;
    use crate::tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };

    struct HookProbePlugin {
        id: &'static str,
        turn_finished_calls: AtomicU32,
        session_ended_calls: AtomicU32,
    }

    impl ToolSpecProvider for HookProbePlugin {}
    impl ToolOverrideHandler for HookProbePlugin {}
    impl PromptSectionProvider for HookProbePlugin {}
    impl MentionCandidateProvider for HookProbePlugin {}

    impl Plugin for HookProbePlugin {
        fn id(&self) -> &str {
            self.id
        }
        fn on_turn_finished(&self, _session: &Session, _turn_start_idx: usize) {
            self.turn_finished_calls.fetch_add(1, Ordering::SeqCst);
        }
        fn on_session_ended(&self, _session: &Session) {
            self.session_ended_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn wait_for(counter: &AtomicU32, expected: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while counter.load(Ordering::SeqCst) != expected {
            assert!(
                Instant::now() < deadline,
                "等待通知落地超时：期望 {expected}，实际 {}",
                counter.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn notify_delivers_hooks_without_blocking_on_slow_plugin() {
        struct SlowPlugin {
            started: AtomicBool,
            finished: AtomicBool,
        }
        impl ToolSpecProvider for SlowPlugin {}
        impl ToolOverrideHandler for SlowPlugin {}
        impl PromptSectionProvider for SlowPlugin {}
        impl MentionCandidateProvider for SlowPlugin {}
        impl Plugin for SlowPlugin {
            fn id(&self) -> &str {
                "slow-notify"
            }
            fn on_session_ended(&self, _session: &Session) {
                self.started.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(2));
                self.finished.store(true, Ordering::SeqCst);
            }
        }

        let slow = Arc::new(SlowPlugin {
            started: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let plugins: Vec<Arc<dyn Plugin>> = vec![slow.clone()];
        let session = Session::new("notify-test");

        let notify_started = Instant::now();
        super::notify_session_ended(&plugins, &session);
        let elapsed = notify_started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "通知投递应立即返回（慢插件不阻塞调用方），实际耗时 {elapsed:?}"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while !slow.started.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "通知应到达慢插件钩子");
            std::thread::sleep(Duration::from_millis(10));
        }
        // 未释放前钩子仍在后台执行，但已不影响调用方。
        assert!(
            !slow.finished.load(Ordering::SeqCst),
            "2 秒阻塞未结束前钩子不应已完成"
        );
    }

    #[test]
    fn notify_survives_plugin_panic() {
        struct PanickingPlugin;
        impl ToolSpecProvider for PanickingPlugin {}
        impl ToolOverrideHandler for PanickingPlugin {}
        impl PromptSectionProvider for PanickingPlugin {}
        impl MentionCandidateProvider for PanickingPlugin {}
        impl Plugin for PanickingPlugin {
            fn id(&self) -> &str {
                "panicking-notify"
            }
            fn on_turn_finished(&self, _session: &Session, _turn_start_idx: usize) {
                panic!("插件钩子 panic 不应打穿通知线程");
            }
        }

        let panicking: Arc<dyn Plugin> = Arc::new(PanickingPlugin);
        let probe = Arc::new(HookProbePlugin {
            id: "probe-notify",
            turn_finished_calls: AtomicU32::new(0),
            session_ended_calls: AtomicU32::new(0),
        });
        let plugins: Vec<Arc<dyn Plugin>> = vec![panicking, probe.clone()];
        let session = Session::new("notify-test");

        std::panic::set_hook(Box::new(|_| {}));
        super::notify_turn_finished(&plugins, &session, 0);
        wait_for(&probe.turn_finished_calls, 1);
        let _ = std::panic::take_hook();

        super::notify_session_ended(&plugins, &session);
        wait_for(&probe.session_ended_calls, 1);
    }
}
