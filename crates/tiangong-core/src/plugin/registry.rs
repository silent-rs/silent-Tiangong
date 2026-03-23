use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, anyhow};

use super::types::{PluginInstance, PluginManifest, PluginStatus};

/// 插件注册表
///
/// 管理插件的发现、注册、注销和查询。
pub struct PluginRegistry {
    /// 已注册的插件（按 ID 索引）
    plugins: HashMap<String, PluginInstance>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// 注册插件
    pub fn register(&mut self, manifest: PluginManifest) -> Result<()> {
        if self.plugins.contains_key(&manifest.id) {
            return Err(anyhow!("插件已注册：{}", manifest.id));
        }
        let id = manifest.id.clone();
        self.plugins.insert(
            id,
            PluginInstance {
                manifest,
                status: PluginStatus::Registered,
                enabled: true,
            },
        );
        Ok(())
    }

    /// 注销插件
    pub fn unregister(&mut self, id: &str) -> Result<PluginInstance> {
        self.plugins
            .remove(id)
            .ok_or_else(|| anyhow!("未找到插件：{id}"))
    }

    /// 获取插件
    pub fn get(&self, id: &str) -> Option<&PluginInstance> {
        self.plugins.get(id)
    }

    /// 获取可变插件引用
    pub fn get_mut(&mut self, id: &str) -> Option<&mut PluginInstance> {
        self.plugins.get_mut(id)
    }

    /// 列出所有插件
    pub fn list(&self) -> Vec<&PluginInstance> {
        self.plugins.values().collect()
    }

    /// 按能力查询可用插件
    pub fn find_by_capability(&self, capability: &str) -> Vec<&PluginInstance> {
        self.plugins
            .values()
            .filter(|instance| {
                instance.enabled
                    && instance.status == PluginStatus::Running
                    && instance
                        .manifest
                        .capabilities
                        .iter()
                        .any(|c| c == capability)
            })
            .collect()
    }

    /// 扫描插件目录，发现可用插件
    ///
    /// 扫描 `~/.tiangong/plugins/` 下的插件清单文件
    pub fn discover_plugins(plugins_dir: &Path) -> Result<Vec<PluginManifest>> {
        let mut manifests = Vec::new();

        if !plugins_dir.exists() {
            return Ok(manifests);
        }

        let entries = std::fs::read_dir(plugins_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let manifest_path = path.join("plugin.json");
                if manifest_path.exists() {
                    let content = std::fs::read_to_string(&manifest_path)?;
                    match serde_json::from_str::<PluginManifest>(&content) {
                        Ok(manifest) => manifests.push(manifest),
                        Err(err) => {
                            tracing::warn!(
                                path = %manifest_path.display(),
                                error = %err,
                                "跳过无效的插件清单"
                            );
                        }
                    }
                }
            }
        }

        Ok(manifests)
    }
}
