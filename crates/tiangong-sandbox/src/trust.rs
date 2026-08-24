//! 插件信任库（RFC 0017 D2–D4）：L3 本地确认记录与 L4 放开开关。
//!
//! 存储于宿主数据目录 `plugin-safety.json`（插件不可达，防自我篡改）。
//! v1 的 L3 确认登记经宿主命令（原生确认对话框 UI 见 RFC 开放问题）；
//! 内容哈希覆盖 plugin.json、sidecar 二进制、wasm 与 dist 入口，
//! 内容变更即授权失效（防"先骗授权再改代码"）。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 信任记录条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPlugin {
    pub plugin_id: String,
    pub content_hash: String,
    /// 本地时间（RFC 3339 风格字符串）。
    pub granted_at: String,
    /// 来源描述（生成目录 / 导入路径）。
    pub origin: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SafetyFile {
    /// L4 放开开关（默认 false；开启后未签名插件可启动 sidecar，审计留痕）。
    #[serde(default)]
    unsafe_mode: bool,
    #[serde(default)]
    unsafe_mode_since: Option<String>,
    /// L3 本地确认记录。
    #[serde(default)]
    trusted: Vec<TrustedPlugin>,
}

/// 插件信任库。
pub struct PluginSafetyStore {
    path: PathBuf,
}

impl PluginSafetyStore {
    /// 打开宿主数据目录下的信任库（不存在则视为空配置）。
    pub fn open(storage_root: &Path) -> Self {
        Self {
            path: storage_root.join("plugin-safety.json"),
        }
    }

    fn load(&self) -> SafetyFile {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, file: &SafetyFile) -> Result<()> {
        let text = serde_json::to_vec_pretty(file).context("序列化信任库失败")?;
        crate::copy::atomic_write(&self.path, &text)
            .with_context(|| self.path.display().to_string())?;
        Ok(())
    }

    /// L4 放开开关状态。
    pub fn unsafe_mode(&self) -> bool {
        self.load().unsafe_mode
    }

    /// 设置 L4 放开开关（开启时记录时间，审计可查）。
    pub fn set_unsafe_mode(&self, enabled: bool) -> Result<()> {
        let mut file = self.load();
        file.unsafe_mode = enabled;
        file.unsafe_mode_since = enabled.then(now_string);
        if enabled {
            tracing::warn!("插件安全校验已关闭（L4 放开开关）");
        }
        self.save(&file)
    }

    /// 全部本地信任记录。
    pub fn trusted_plugins(&self) -> Vec<TrustedPlugin> {
        self.load().trusted
    }

    /// 登记本地信任（以当前插件目录内容哈希锁定）。
    pub fn grant(&self, plugin_id: &str, directory: &Path, origin: &str) -> Result<TrustedPlugin> {
        let entry = TrustedPlugin {
            plugin_id: plugin_id.to_string(),
            content_hash: plugin_content_hash(directory),
            granted_at: now_string(),
            origin: origin.to_string(),
        };
        let mut file = self.load();
        file.trusted.retain(|item| item.plugin_id != plugin_id);
        file.trusted.push(entry.clone());
        self.save(&file)?;
        tracing::warn!(plugin_id, origin, "登记本地信任插件（L3）");
        Ok(entry)
    }

    /// 撤销本地信任。
    pub fn revoke(&self, plugin_id: &str) -> Result<()> {
        let mut file = self.load();
        file.trusted.retain(|item| item.plugin_id != plugin_id);
        self.save(&file)
    }

    /// L3 判定：插件当前内容哈希与登记一致才视为受信（内容变更即失效）。
    pub fn is_trusted(&self, plugin_id: &str, directory: &Path) -> bool {
        let current = plugin_content_hash(directory);
        self.load()
            .trusted
            .iter()
            .any(|item| item.plugin_id == plugin_id && item.content_hash == current)
    }
}

/// 插件目录内容哈希：manifest、sidecar 二进制、wasm、dist 入口（存在的）
/// 逐文件 sha256 后整体再哈希。文件顺序固定，保证结果稳定。
pub fn plugin_content_hash(directory: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(directory.to_string_lossy().as_bytes());
    if let Ok(manifest_text) = fs::read_to_string(directory.join("plugin.json")) {
        hasher.update(b"plugin.json:");
        hasher.update(strip_volatile_fields(&manifest_text).as_bytes());
    }
    for entry in walk_files(directory, directory) {
        hasher.update(entry.as_bytes());
        hasher.update(b"\0");
        match fs::read(directory.join(&entry)) {
            Ok(bytes) => hasher.update(&bytes),
            Err(error) => {
                // 读取失败不能静默跳过：以错误标记参与哈希并告警，
                // 保证授权前后状态差异可被检测。
                tracing::warn!(entry = %entry, %error, "信任哈希扫描读取失败");
                hasher.update(b"<unreadable>");
            }
        }
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

/// 排除清单中的易变字段（version 变更不应使授权失效以外——保守起见 v1 不排除
/// 任何字段；保留函数占位供后续按需放宽）。
fn strip_volatile_fields(text: &str) -> String {
    text.to_string()
}

/// 完整扫描插件目录（无深度上限）：符号链接不跟随（防环），运行产物目录
/// （runtime/logs/data）与 node_modules 排除——依赖内容经锁文件
/// （package-lock.json / yarn.lock 等，位于扫描范围内）锚定。
fn walk_files(base: &Path, dir: &Path) -> Vec<String> {
    const IGNORED: [&str; 4] = ["runtime", "logs", "data", "node_modules"];
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut names: Vec<_> = entries.flatten().collect();
    names.sort_by_key(|entry| entry.file_name());
    for entry in names {
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORED.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        // symlink_metadata 判真实类型：符号链接一律按文件路径记录、不跟随。
        let is_dir = fs::symlink_metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if is_dir {
            out.extend(walk_files(base, &path));
        } else {
            out.push(
                path.strip_prefix(base)
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                    .unwrap_or(name.clone()),
            );
        }
    }
    out
}

fn now_string() -> String {
    chrono::Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_trust_and_detect_content_change() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("my-plugin");
        fs::create_dir_all(plugin_dir.join("dist")).unwrap();
        fs::write(plugin_dir.join("plugin.json"), r#"{"id":"my-plugin"}"#).unwrap();
        fs::write(plugin_dir.join("dist/index.html"), "<html></html>").unwrap();

        let store = PluginSafetyStore::open(root.path());
        assert!(!store.is_trusted("my-plugin", &plugin_dir));

        store.grant("my-plugin", &plugin_dir, "/tmp/dev").unwrap();
        assert!(store.is_trusted("my-plugin", &plugin_dir));
        assert_eq!(store.trusted_plugins().len(), 1);

        // 内容变更 → 授权失效。
        fs::write(plugin_dir.join("dist/index.html"), "<html>changed</html>").unwrap();
        assert!(!store.is_trusted("my-plugin", &plugin_dir));

        // 撤销。
        store.grant("my-plugin", &plugin_dir, "/tmp/dev").unwrap();
        store.revoke("my-plugin").unwrap();
        assert!(store.trusted_plugins().is_empty());
    }

    #[test]
    fn deep_nested_files_are_covered() {
        let root = tempfile::tempdir().unwrap();
        let plugin_dir = root.path().join("deep-plugin");
        // 五层嵌套（超过旧版深度上限 4）。
        let deep = plugin_dir.join("a/b/c/d/e");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("payload.js"), "original").unwrap();

        let store = PluginSafetyStore::open(root.path());
        store.grant("deep-plugin", &plugin_dir, "/tmp").unwrap();
        assert!(store.is_trusted("deep-plugin", &plugin_dir));

        // 深层文件变更必须使授权失效。
        fs::write(deep.join("payload.js"), "tampered").unwrap();
        assert!(!store.is_trusted("deep-plugin", &plugin_dir));
    }

    #[test]
    fn unsafe_mode_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let store = PluginSafetyStore::open(root.path());
        assert!(!store.unsafe_mode());
        store.set_unsafe_mode(true).unwrap();
        assert!(store.unsafe_mode());
        store.set_unsafe_mode(false).unwrap();
        assert!(!store.unsafe_mode());
    }
}
