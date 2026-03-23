use anyhow::{Result, anyhow};

use super::registry::PluginRegistry;
use super::types::PluginStatus;

/// 插件生命周期管理器
///
/// 负责插件进程的启动、停止、健康检查和重启。
pub struct PluginLifecycleManager {
    /// 最大重启次数
    max_restart_attempts: usize,
}

impl Default for PluginLifecycleManager {
    fn default() -> Self {
        Self {
            max_restart_attempts: 3,
        }
    }
}

impl PluginLifecycleManager {
    pub fn new(max_restart_attempts: usize) -> Self {
        Self {
            max_restart_attempts,
        }
    }

    /// 启动插件
    ///
    /// 根据插件的传输方式（Stdio/Http）启动对应进程
    pub fn start_plugin(&self, registry: &mut PluginRegistry, plugin_id: &str) -> Result<()> {
        let instance = registry
            .get_mut(plugin_id)
            .ok_or_else(|| anyhow!("未找到插件：{plugin_id}"))?;

        if !instance.enabled {
            return Err(anyhow!("插件已禁用：{plugin_id}"));
        }

        if instance.status == PluginStatus::Running {
            return Ok(());
        }

        instance.status = PluginStatus::Starting;

        // TODO: 根据 transport 类型实际启动进程
        // 当前仅标记状态，实际的进程管理将在后续版本实现
        instance.status = PluginStatus::Running;

        tracing::info!(plugin_id = plugin_id, "插件已启动");
        Ok(())
    }

    /// 停止插件
    pub fn stop_plugin(&self, registry: &mut PluginRegistry, plugin_id: &str) -> Result<()> {
        let instance = registry
            .get_mut(plugin_id)
            .ok_or_else(|| anyhow!("未找到插件：{plugin_id}"))?;

        if instance.status != PluginStatus::Running {
            return Ok(());
        }

        // TODO: 实际停止进程
        instance.status = PluginStatus::Stopped;

        tracing::info!(plugin_id = plugin_id, "插件已停止");
        Ok(())
    }

    /// 健康检查
    pub fn health_check(&self, registry: &PluginRegistry, plugin_id: &str) -> Result<bool> {
        let instance = registry
            .get(plugin_id)
            .ok_or_else(|| anyhow!("未找到插件：{plugin_id}"))?;

        Ok(instance.status == PluginStatus::Running)
    }

    /// 重启插件（带重试策略）
    pub fn restart_plugin(&self, registry: &mut PluginRegistry, plugin_id: &str) -> Result<()> {
        self.stop_plugin(registry, plugin_id)?;

        for attempt in 1..=self.max_restart_attempts {
            match self.start_plugin(registry, plugin_id) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!(
                        plugin_id = plugin_id,
                        attempt = attempt,
                        max_attempts = self.max_restart_attempts,
                        error = %err,
                        "插件重启失败，将重试"
                    );
                    if attempt == self.max_restart_attempts {
                        if let Some(instance) = registry.get_mut(plugin_id) {
                            instance.status = PluginStatus::Failed;
                        }
                        return Err(anyhow!(
                            "插件 {plugin_id} 重启失败，已达最大重试次数 {}",
                            self.max_restart_attempts
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}
