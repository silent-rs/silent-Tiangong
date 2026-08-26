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
use sha2::Digest;

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
    // 原生确认通过：为解释器 sidecar 落本地信任锚（内容清单整体哈希）。
    if manifest.sidecar.is_some() {
        write_local_trust(staged.path())?;
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

/// 在暂存目录写入本地信任标记：锚定当前内容清单的整体哈希。
///
/// 信任语义：用户经原生确认对话框亲手安装的本地插件，其解释器 sidecar
/// 允许以 stdio 常驻运行；安装后任何文件与清单不一致即拒绝启动。
/// 仅当构建产物带 content-manifest.json（devkit build 必然生成）时可落锚。
fn write_local_trust(staged_path: &Path) -> Result<()> {
    let manifest_path = staged_path.join("content-manifest.json");
    // 锚定前先双向校验：清单必须完整覆盖暂存目录的受管文件树且哈希一致，
    // 不完整清单（漏列文件）在安装时即拒绝，不给"未列出文件可被替换"留通道。
    crate::sidecar::SidecarConfig::verify_integrity_manifest(&manifest_path, staged_path)?;
    let raw = std::fs::read(&manifest_path).with_context(|| {
        "本地安装缺少内容清单（content-manifest.json），无法建立本地信任；请用 plugin-creator 重新构建".to_string()
    })?;
    let anchor = hex::encode(sha2::Sha256::digest(&raw));
    let trust = serde_json::json!({
        "kind": "local-confirm",
        "content_sha256": anchor,
        "created_at": chrono::Local::now().naive_local().to_string(),
    });
    std::fs::write(
        staged_path.join("local-trust.json"),
        serde_json::to_vec_pretty(&trust)?,
    )
    .with_context(|| "写入本地信任标记失败".to_string())?;
    Ok(())
}

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

    /// 安装完整链（功能验证）：确认桩 → 暂存（不可变副本）→ 导入 → 安装目录
    /// 与注册表就位；确认信息（版本）来自暂存副本而非可变的 release/。
    #[test]
    fn install_完整链_暂存确认导入与注册表() {
        let root = tempfile::tempdir().unwrap();
        tiangong_config::registry::init_from_dir(root.path());
        let project = root.path().join(PLUGIN_DEV_DIR).join("inst-demo");
        std::fs::create_dir_all(project.join("release/dist")).unwrap();
        std::fs::write(
            project.join(PROJECT_META_FILE),
            r#"{"plugin_id":"inst-demo","name":"安装链验证","template":"ts-npx","created_at":"t"}"#,
        )
        .unwrap();
        std::fs::write(
            project.join("release/plugin.json"),
            r#"{"schema_version":2,"id":"inst-demo","version":"9.9.9","permissions":[],"entrypoints":["desktop"],"capabilities":{"prompt":true},"prompt":["能力说明"],"mention":{"hint":"安装链验证能力"},"ui":{"contributions":[{"slot":"extension.tab","id":"app","title":"安装链验证","entry":"dist/index.html"}]}}"#,
        )
        .unwrap();
        std::fs::write(project.join("release/dist/index.html"), "<html></html>").unwrap();

        let confirmed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&confirmed);
        set_plugin_dev_install_confirm(Arc::new(move |request: &InstallRequest| {
            sink.lock()
                .unwrap()
                .push((request.plugin_id.clone(), request.version.clone()));
            true
        }));
        let result = install(root.path(), "inst-demo").expect("安装完整链");
        assert_eq!(result.plugin_id, "inst-demo");
        assert_eq!(result.version, "9.9.9");
        assert!(result.enabled, "安装后应启用");
        // 确认信息来自暂存副本（版本正确）
        let confirmed = confirmed.lock().unwrap();
        assert_eq!(
            confirmed.as_slice(),
            [("inst-demo".to_string(), "9.9.9".to_string())]
        );
        // 安装目录与注册表
        assert!(root.path().join("plugins/inst-demo/plugin.json").is_file());
        assert!(
            root.path()
                .join("plugins/inst-demo/dist/index.html")
                .is_file()
        );
        assert!(
            crate::registry::plugin_manifest("inst-demo").is_some(),
            "注册表应可见已装插件"
        );
        // @提及候选实时聚合：安装后立即可见（不依赖会话 Core 快照）。
        let mentions = crate::registry::collect_mention_candidates();
        assert!(
            mentions
                .iter()
                .any(|candidate| candidate.value == "@plugin:inst-demo"),
            "安装后 mention 候选应立即可见：{mentions:?}"
        );
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

    fn find_node_for_test() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("TIANGONG_NODE_PATH") {
            let path = PathBuf::from(path);
            return path.is_file().then_some(path);
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|directory| directory.join(if cfg!(windows) { "node.exe" } else { "node" }))
            .find(|path| path.is_file())
    }

    /// 构造 devkit 风格的 node sidecar 构建产物（含内容哈希清单）。
    fn make_node_sidecar_release(root: &Path, id: &str) -> PathBuf {
        let release = root.join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("app")).unwrap();
        std::fs::create_dir_all(release.join("sidecar/vendor/tiangong-sidecar-sdk")).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            format!(
                r#"{{"schema_version":2,"id":"{id}","version":"0.1.0","permissions":["sidecar.invoke"],"ui":{{"contributions":[{{"slot":"extension.tab","id":"app","entry":"app/index.html"}}]}},"sidecar":{{"runtime":"node","entry":"sidecar/main.mjs"}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(release.join("app/index.html"), "<html></html>").unwrap();
        let sdk =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/sdk-sidecar/index.mjs");
        std::fs::copy(
            &sdk,
            release.join("sidecar/vendor/tiangong-sidecar-sdk/index.mjs"),
        )
        .unwrap();
        std::fs::write(
            release.join("sidecar/main.mjs"),
            r#"
import { runSidecar } from './vendor/tiangong-sidecar-sdk/index.mjs';
await runSidecar({
  pluginId: 'ID_PLACEHOLDER',
  pluginVersion: '0.1.0',
  dispatch(operation, payload) {
    if (operation === 'demo.echo') {
      return { payload: { text: payload?.text ?? '' } };
    }
    return { payload: {} };
  },
});
"#
            .replace("ID_PLACEHOLDER", id),
        )
        .unwrap();
        write_content_manifest(&release);
        release
    }

    /// devkit build 同款内容清单：release 全树（排除清单自身）路径 + sha256。
    fn write_content_manifest(release: &Path) {
        use sha2::Digest;
        let mut files = Vec::new();
        let mut stack = vec![release.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                } else {
                    let path = entry.path();
                    let relative = path.strip_prefix(release).unwrap().display().to_string();
                    if relative == "content-manifest.json" {
                        continue;
                    }
                    let raw = std::fs::read(&path).unwrap();
                    files.push(json!({
                        "path": relative.replace('\\', "/"),
                        "sha256": hex::encode(sha2::Sha256::digest(&raw)),
                    }));
                }
            }
        }
        std::fs::write(
            release.join("content-manifest.json"),
            json!({"algorithm": "sha256", "files": files}).to_string(),
        )
        .unwrap();
    }

    fn copy_tree_for_test(source: &Path, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree_for_test(&entry.path(), &destination);
            } else {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
    }

    #[test]
    fn 解释器sidecar_本地信任安装_真实调用与篡改拒绝() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let id = "node-sc-demo";
        make_project(root.path(), id);
        make_node_sidecar_release(root.path(), id);

        set_plugin_dev_install_confirm(Arc::new(|_: &InstallRequest| true));
        let result = install(root.path(), id).expect("解释器 sidecar 安装");
        assert_eq!(result.plugin_id, id);

        // 真实调用：宿主 stdio 连接 ↔ node 解释器 sidecar。
        let response = crate::registry::invoke_sidecar(
            root.path(),
            id,
            "demo.echo",
            json!({"text": "本地信任"}),
        )
        .expect("sidecar 调用");
        assert_eq!(response["text"], "本地信任");

        // 篡改安装目录内的入口脚本：内容清单复核必须拒绝。
        let installed_entry = root
            .path()
            .join("plugins")
            .join(id)
            .join("sidecar/main.mjs");
        std::fs::write(&installed_entry, "// tampered\n").unwrap();
        let error = crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({}))
            .expect_err("篡改后应拒绝启动");
        assert!(
            format!("{error:#}").contains("篡改"),
            "应报篡改错误：{error:#}"
        );
    }

    #[test]
    fn 解释器sidecar_无本地信任时拒绝启动() {
        let root = tempfile::tempdir().unwrap();
        let id = "node-sc-notrusted";
        make_project(root.path(), id);
        let release = make_node_sidecar_release(root.path(), id);
        // 手动把产物放进安装目录，不经 plugin-dev 确认链（无 local-trust.json）。
        let installed = root.path().join("plugins").join(id);
        copy_tree_for_test(&release, &installed);
        let error = crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({}))
            .expect_err("未建立本地信任应拒绝");
        assert!(
            format!("{error:#}").contains("本地信任"),
            "应提示本地信任安装：{error:#}"
        );
    }

    /// 真实 creator 产物全链路：package → plugin-dev 安装（原生确认 + 暂存 +
    /// 双向完整性 + 本地信任落锚）→ 按需 sidecar 真实执行 devkit.init（验证
    /// templates 随行资源经 resources 声明进入安装目录并被 devkit 使用）。
    /// 产物（release/）由 `yarn package` 生成、不入库：CI 无产物时跳过。
    #[test]
    fn creator真实产物_安装与devkit_init全链路() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let release_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/tiangong-plugin-creator/release");
        if !release_source.join("plugin.json").is_file() {
            eprintln!(
                "跳过：缺少真实构建产物 {}（先 yarn package）",
                release_source.display()
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        // 安装链的可用性探测读取全局模型配置；测试进程用隔离目录初始化。
        tiangong_config::registry::init_from_dir(&root.path().join("config"));
        let id = "plugin-creator";
        let dev_root = root.path().join(PLUGIN_DEV_DIR).join(id);
        copy_tree_for_test(&release_source, &dev_root.join("release"));
        make_project(root.path(), id);

        set_plugin_dev_install_confirm(Arc::new(|_: &InstallRequest| true));
        let result = install(root.path(), id).expect("creator 真实产物安装");
        assert_eq!(result.plugin_id, id);

        // 按需 sidecar 真实执行 devkit.init：安装目录内的 templates 必须可用。
        let init_root = root.path().join("init-output");
        let response = crate::registry::invoke_sidecar(
            root.path(),
            id,
            "devkit.init",
            json!({"args": ["ui-app", "chain-probe", "--name", "链路探针"], "root": init_root}),
        )
        .expect("devkit.init 经按需 sidecar 执行");
        assert_eq!(
            response["ok"],
            json!(true),
            "devkit.init 应成功: {response}"
        );
        assert!(
            init_root.join("chain-probe/plugin.json").is_file(),
            "模板项目应真实创建（templates 随行资源可用）"
        );
    }
    /// 完整用户旅程（显式运行：`cargo test -p tiangong-plugin-runtime --lib
    /// -- --ignored`）：经 creator 的按需 sidecar 从零创建 node-sidecar 插件
    /// → 注入自定义操作 → devkit 真实构建（yarn 工程链）→ 原生确认安装 →
    /// 宿主连接新插件的 sidecar 真实调用。全程不经 GUI，等价于 Agent 操作序列。
    #[test]
    #[ignore = "真实 yarn 构建需数分钟与网络，按需显式运行"]
    fn 从零创建node_sidecar插件_完整旅程() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let release_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/tiangong-plugin-creator/release");
        if !release_source.join("plugin.json").is_file() {
            eprintln!("跳过：缺少真实构建产物（先在 creator 目录 yarn package）");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        tiangong_config::registry::init_from_dir(&root.path().join("config"));
        let creator = "plugin-creator";
        let dev_root = root.path().join(PLUGIN_DEV_DIR).join(creator);
        copy_tree_for_test(&release_source, &dev_root.join("release"));
        make_project(root.path(), creator);
        set_plugin_dev_install_confirm(Arc::new(|_: &InstallRequest| true));
        install(root.path(), creator).expect("creator 安装");

        let devkit = |command: &str, args: serde_json::Value| {
            crate::registry::invoke_sidecar(
                root.path(),
                creator,
                &format!("devkit.{command}"),
                args,
            )
        };

        // 1) 初始化 node-sidecar 模板项目（真实模板，写入受控开发根）。
        let project_id = "journey-demo";
        let response = devkit(
            "init",
            json!({"args": ["node-sidecar", project_id, "--name", "旅程验证"], "root": root.path().join(PLUGIN_DEV_DIR)}),
        )
        .expect("devkit.init");
        assert_eq!(response["ok"], json!(true), "init 失败: {response}");

        // 2) 修改 sidecar 源码：注入自定义操作（模拟用户/Agent 编码）。
        let entry = root
            .path()
            .join(PLUGIN_DEV_DIR)
            .join(project_id)
            .join("sidecar/main.mjs");
        let source = std::fs::read_to_string(&entry).unwrap();
        let patched = source.replace(
            "    if (operation === 'demo.echo') {",
            "    if (operation === 'journey.greet') {\n      return { payload: { message: `你好，${payload?.who ?? '天工'}！` } };\n    }\n    if (operation === 'demo.echo') {",
        );
        assert!(patched != source, "应成功注入自定义操作");
        std::fs::write(&entry, patched).unwrap();

        // 3) 真实构建（yarn install + 类型检查 + vite/esbuild 双端打包）。
        let response = devkit(
            "build",
            json!({"args": [project_id], "root": root.path().join(PLUGIN_DEV_DIR)}),
        )
        .expect("devkit.build");
        assert_eq!(response["ok"], json!(true), "build 失败: {response}");

        // 4) 安装新插件（原生确认通道 + 完整性 + 本地信任）。
        let result = install(root.path(), project_id).expect("新插件安装");
        assert_eq!(result.plugin_id, project_id);

        // 5) 宿主连接新插件 sidecar，真实调用自定义操作。
        let response = crate::registry::invoke_sidecar(
            root.path(),
            project_id,
            "journey.greet",
            json!({"who": "完整旅程"}),
        )
        .expect("自定义操作调用");
        assert_eq!(
            response["message"],
            json!("你好，完整旅程！"),
            "响应: {response}"
        );
    }
}
