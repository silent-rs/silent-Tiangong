//! run_command / run_shell 进程内插件（基础命令执行能力）。
//!
//! 提供受控命令执行（含命令白名单、路径越界校验、shell 脚本派生），供 CLI / Server
//! 入口使用；GUI 入口使用 terminal 插件（PTY 执行 + 命令回显）。
//!
//! 与 terminal 插件走不同流程：本插件用 tokio::process::Command 子进程执行（无 PTY），
//! terminal 插件经嵌入式终端面板 PTY 执行。

pub mod handler;
pub mod plugin;

pub use plugin::CommandPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造 command 插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(CommandPlugin::new())
}

/// 构造默认的插件列表，供 CLI / Server 入口注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
