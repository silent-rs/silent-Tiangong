//! plugin-dev 受限桥接服务（plugin creator 的宿主承载，外置化后的瘦形态）。
//!
//! 业务工具链（init / validate / build / run / logs）已外置到 npm 包
//! `@silent-ai/plugin-creator`（devkit CLI），由 Agent 经命令通道执行
//! `npx -y @silent-ai/plugin-creator@<版本> <命令>`——沙箱、联网审批、
//! 会话信任复用命令通道现成机制。宿主只保留必须落在宿主侧的操作：
//!
//! - `install`：构建产物写入 plugins/ 注册表 + 原生确认（fail-closed）
//!   ——插件无法自行完成安装与授权，用户是唯一授权主体；
//! - `list` / `status`：纯只读查询（开发目录的元数据与版本状态）。
//!
//! 写范围锁定开发目录 `~/.tiangong/plugins-dev/`，不可触达信任库、公钥库
//! 与宿主设置。服务保持插件中立，任何声明 `plugin-dev.use` 权限的插件
//! 均可复用本通道。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

/// 开发目录名（位于存储根下，与 plugins/ 平级）。
pub const PLUGIN_DEV_DIR: &str = "plugins-dev";
/// 项目元数据文件名（位于项目目录根部）。
const PROJECT_META_FILE: &str = ".plugin-dev.json";

/// 安装确认请求（原生确认对话框的展示内容）。
#[derive(Debug, Clone, Serialize)]
pub struct InstallRequest {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub permissions: Vec<String>,
    /// 待安装内容所在目录。
    pub directory: String,
}

/// 安装确认回调：返回 false 视为用户拒绝。必须由宿主原生对话框实现。
pub type InstallConfirmHandler = Arc<dyn Fn(&InstallRequest) -> bool + Send + Sync>;

static INSTALL_CONFIRM: OnceLock<InstallConfirmHandler> = OnceLock::new();

/// 注入安装确认回调（桌面入口启动时调用）。
pub fn set_plugin_dev_install_confirm(handler: InstallConfirmHandler) {
    let _ = INSTALL_CONFIRM.set(handler);
}

/// 处理一次 `plugin-dev.*` 桥接调用（权限校验由 bridge 层完成）。
pub fn call(plugin_id: &str, method: &str, payload: &str) -> Result<String> {
    let install_dir = crate::registry::plugin_install_directory(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("plugin-dev 调用方插件 {plugin_id} 未加载"))?;
    let storage_root = storage_root_of(&install_dir)?;
    let operation = method.strip_prefix("plugin-dev.").unwrap_or_default();
    let result = match operation {
        "install" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(install(&storage_root, &request.id)?)?
        }
        "list" => serde_json::to_value(list(&storage_root)?)?,
        "status" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(status(&storage_root, &request.id)?)?
        }
        _ => bail!(
            "plugin-dev 未知方法 {method}（宿主仅提供 install/list/status；\
             init/validate/build/run/logs 经命令通道执行 @silent-ai/plugin-creator devkit）"
        ),
    };
    serde_json::to_string(&result).context("序列化 plugin-dev 结果失败")
}

fn parse_payload<T: for<'de> Deserialize<'de>>(payload: &str) -> Result<T> {
    serde_json::from_str(payload).context("plugin-dev 请求负载必须是合法 JSON 对象")
}

fn storage_root_of(install_dir: &Path) -> Result<PathBuf> {
    install_dir
        .parent()
        .and_then(|plugins_dir| plugins_dir.parent())
        .map(Path::to_path_buf)
        .context("无法定位插件存储根")
}

// ── 请求/响应类型 ──

#[derive(Debug, Deserialize)]
struct IdRequest {
    id: String,
}

#[derive(Debug, Serialize)]
struct ProjectEntry {
    id: String,
    name: String,
    template: String,
    /// 项目源码 plugin.json 版本（源码态）。
    source_version: Option<String>,
    /// release/ 构建产物版本（None 表示尚未构建）。
    release_version: Option<String>,
    /// 已安装版本（None 表示未安装）。
    installed_version: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct InstallResult {
    plugin_id: String,
    version: String,
    state: String,
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct StatusResult {
    exists: bool,
    template: Option<String>,
    name: Option<String>,
    source_version: Option<String>,
    release_version: Option<String>,
    installed_version: Option<String>,
    /// release 产物与源码版本一致且已安装同版本。
    up_to_date: bool,
}

// ── list ──

fn list(storage_root: &Path) -> Result<Vec<ProjectEntry>> {
    let dev_root = storage_root.join(PLUGIN_DEV_DIR);
    let mut entries = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&dev_root) else {
        return Ok(entries);
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(PROJECT_META_FILE).is_file() {
            continue;
        }
        let Ok(meta) = read_project_meta(&path) else {
            continue;
        };
        let id = meta.plugin_id;
        let source_version = read_manifest_version(&path.join("plugin.json"));
        let release_version = read_manifest_version(&path.join("release/plugin.json"));
        let installed_version =
            installed_plugin_manifest(storage_root, &id).map(|manifest| manifest.version);
        entries.push(ProjectEntry {
            name: meta.name,
            template: meta.template,
            created_at: meta.created_at,
            source_version,
            release_version,
            installed_version,
            id,
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

#[derive(Debug, Deserialize)]
struct ProjectMeta {
    plugin_id: String,
    name: String,
    template: String,
    #[serde(default)]
    created_at: Option<String>,
}

fn read_project_meta(project_dir: &Path) -> Result<ProjectMeta> {
    let content =
        std::fs::read_to_string(project_dir.join(PROJECT_META_FILE)).with_context(|| {
            format!(
                "读取项目元数据失败：{}",
                project_dir.join(PROJECT_META_FILE).display()
            )
        })?;
    Ok(serde_json::from_str(&content)?)
}

fn read_manifest_version(path: &Path) -> Option<String> {
    PluginManifest::load(path)
        .ok()
        .map(|manifest| manifest.version)
}

fn installed_plugin_manifest(storage_root: &Path, plugin_id: &str) -> Option<PluginManifest> {
    PluginManifest::load(
        &storage_root
            .join("plugins")
            .join(plugin_id)
            .join("plugin.json"),
    )
    .ok()
}

// ── install ──

fn install(storage_root: &Path, project_id: &str) -> Result<InstallResult> {
    let project_dir = dev_project_dir(storage_root, project_id)?;
    let release_dir = project_dir.join("release");
    let release_manifest = release_dir.join("plugin.json");
    if !release_manifest.is_file() {
        bail!(
            "项目 {project_id} 尚无构建产物（{} 不存在），先执行构建\
            （npx -y @silent-ai/plugin-creator@1.0.0 build {project_id}）",
            release_manifest.display()
        );
    }
    // 先暂存（不可变事务副本），确认与导入都作用于这份副本——外置化后
    // devkit 是独立进程，宿主无法锁住 release/ 的变化；若先确认后暂存，
    // 用户确认窗口期内 release/ 可被替换，形成"确认内容 ≠ 安装内容"。
    // StagedPlugin 的 Drop 保证用户拒绝/任意失败路径自动清理暂存目录。
    let staged = crate::artifacts::stage_local_plugin(storage_root, &release_dir)?;
    let staged_manifest = staged.path().join("plugin.json");
    let manifest = PluginManifest::load(&staged_manifest)
        .with_context(|| format!("构建产物清单无效: {}", staged_manifest.display()))?;
    if manifest.id != project_id {
        bail!(
            "构建产物清单 ID {} 与项目 ID {project_id} 不一致，请检查 plugin.json",
            manifest.id
        );
    }
    let name = manifest
        .ui
        .as_ref()
        .and_then(|ui| ui.contributions.first())
        .map(|contribution| {
            if contribution.title.is_empty() {
                contribution.id.clone()
            } else {
                contribution.title.clone()
            }
        })
        .unwrap_or_else(|| manifest.id.clone());
    let request = InstallRequest {
        plugin_id: manifest.id.clone(),
        name,
        version: manifest.version.clone(),
        permissions: manifest.permissions.clone(),
        directory: release_dir.display().to_string(),
    };
    let Some(confirm) = INSTALL_CONFIRM.get() else {
        bail!("宿主未接入原生安装确认，拒绝安装（fail-closed）");
    };
    if !confirm(&request) {
        bail!("用户取消了插件 {} 的安装", request.plugin_id);
    }
    // 暂存/导入事务由安装链的 LOAD_OPERATION 全局锁串行化；
    // 确认等待不持锁（避免阻塞其它插件的加载操作）。
    let status = crate::registry::import_staged_plugin(storage_root, staged.path())?;
    tracing::info!(plugin = %status.id, version = %status.manifest_version, "plugin-dev 安装完成");
    Ok(InstallResult {
        plugin_id: status.id,
        version: status.manifest_version,
        state: status.state,
        enabled: status.enabled,
    })
}

// ── status ──

fn status(storage_root: &Path, project_id: &str) -> Result<StatusResult> {
    let project_dir = dev_project_dir(storage_root, project_id)?;
    if !project_dir.join(PROJECT_META_FILE).is_file() {
        return Ok(StatusResult {
            exists: false,
            template: None,
            name: None,
            source_version: None,
            release_version: None,
            installed_version: None,
            up_to_date: false,
        });
    }
    let meta = read_project_meta(&project_dir)?;
    let source_version = read_manifest_version(&project_dir.join("plugin.json"));
    let release_version = read_manifest_version(&project_dir.join("release/plugin.json"));
    let installed_version =
        installed_plugin_manifest(storage_root, &meta.plugin_id).map(|manifest| manifest.version);
    let up_to_date = source_version.is_some()
        && source_version == release_version
        && installed_version == source_version;
    Ok(StatusResult {
        exists: true,
        template: Some(meta.template),
        name: Some(meta.name),
        source_version,
        release_version,
        installed_version,
        up_to_date,
    })
}

// ── 公共防护 ──

/// 项目/插件 ID 白名单（与 manifest id 规则一致），防路径逃逸。
fn validate_project_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("插件 ID 只能包含 ASCII 字母数字与 - _ .：{id}");
    }
    Ok(())
}

/// 开发项目目录（canonicalize 校验仍在 plugins-dev 内）。
fn dev_project_dir(storage_root: &Path, project_id: &str) -> Result<PathBuf> {
    validate_project_id(project_id)?;
    let dev_root = storage_root.join(PLUGIN_DEV_DIR);
    std::fs::create_dir_all(&dev_root)
        .with_context(|| format!("创建开发根目录失败: {}", dev_root.display()))?;
    let project_dir = dev_root.join(project_id);
    let canonical_root = dev_root.canonicalize().context("开发根目录规范化失败")?;
    if let Ok(canonical) = project_dir.canonicalize()
        && !canonical.starts_with(&canonical_root)
    {
        bail!("项目路径越界: {}", project_dir.display());
    }
    Ok(project_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_project(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(PLUGIN_DEV_DIR).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(PROJECT_META_FILE),
            json!({"plugin_id": id, "name": id, "template": "ts-npx", "created_at": "2026-08-25 00:00:00"})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"x","version":"0.2.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"app/index.html"}]}}"#
                .replace("\"x\"", &format!("\"{id}\"")),
        )
        .unwrap();
        dir
    }

    #[test]
    fn 项目id白名单拒绝路径逃逸() {
        assert!(validate_project_id("my-plugin_1.0").is_ok());
        for bad in ["", ".", "..", "../escape", "a/b", "a\\b", "空 格"] {
            assert!(validate_project_id(bad).is_err(), "应当拒绝 {bad:?}");
        }
    }

    #[test]
    fn list_返回项目与版本状态() {
        let root = tempfile::tempdir().unwrap();
        make_project(root.path(), "demo");
        std::fs::create_dir_all(
            root.path()
                .join(PLUGIN_DEV_DIR)
                .join("demo")
                .join("release"),
        )
        .unwrap();
        std::fs::write(
            root.path().join(PLUGIN_DEV_DIR).join("demo").join("release/plugin.json"),
            r#"{"schema_version":2,"id":"demo","version":"0.1.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"app/index.html"}]}}"#,
        )
        .unwrap();
        let entries = list(root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "demo");
        assert_eq!(entries[0].source_version.as_deref(), Some("0.2.0"));
        assert_eq!(entries[0].release_version.as_deref(), Some("0.1.0"));
        assert!(entries[0].installed_version.is_none());
    }

    #[test]
    fn status_未初始化返回不存在() {
        let root = tempfile::tempdir().unwrap();
        let result = status(root.path(), "ghost").unwrap();
        assert!(!result.exists);
    }

    #[test]
    fn install_缺构建产物时给出devkit指引() {
        let root = tempfile::tempdir().unwrap();
        make_project(root.path(), "demo");
        let error = install(root.path(), "demo").unwrap_err();
        assert!(
            error.to_string().contains("@silent-ai/plugin-creator"),
            "应指引 devkit 命令：{error}"
        );
    }
}
