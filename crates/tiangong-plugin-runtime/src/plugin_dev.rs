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
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

/// 开发目录名（位于存储根下，与 plugins/ 平级）。
pub const PLUGIN_DEV_DIR: &str = "plugins-dev";
/// 项目元数据文件名（位于项目目录根部）。
const PROJECT_META_FILE: &str = ".plugin-dev.json";

/// 处理一次 `plugin-dev.*` 桥接调用（权限校验由 bridge 层完成）。
/// 自动签名安装的调用方身份（判定依据由宿主注入的策略消费）。
pub struct InstallerIdentity {
    pub plugin_id: String,
    /// 调用方插件当前持有官方签名（发布者为官方保留标识，来自宿主侧
    /// 注册表数据，不由前端传入）。
    pub official_signed: bool,
}

/// 受信安装方判定：返回 true 才允许经桥接触发用户密钥自动签名安装。
/// 由宿主启动时注入（策略含具体插件身份，runtime 保持插件中立）。
pub type TrustedInstallerHandler = Arc<dyn Fn(&InstallerIdentity) -> bool + Send + Sync>;

static TRUSTED_INSTALLER: std::sync::RwLock<Option<TrustedInstallerHandler>> =
    std::sync::RwLock::new(None);

/// 注入受信安装方判定（覆盖语义；缺失时 install fail-closed）。
pub fn set_plugin_dev_trusted_installer(handler: TrustedInstallerHandler) {
    if let Ok(mut current) = TRUSTED_INSTALLER.write() {
        *current = Some(handler);
    }
}

/// 清除受信安装方判定（测试隔离用：恢复 fail-closed 初始态）。
#[cfg(test)]
pub(crate) fn clear_plugin_dev_trusted_installer_for_test() {
    if let Ok(mut current) = TRUSTED_INSTALLER.write() {
        *current = None;
    }
}

/// 受信构建登记：宿主观察者登记「受信插件的 sidecar 真实构建产出」的
/// 项目。install 只接受有登记的项目——自动签名的授权对象是「使用 Creator
/// 开发的产物」，产物必须经宿主进程内发起的真实构建，堵住前端自报身份
/// 冒充安装任意目录内容的通道。
type TrustedBuildKey = (String, String);
type TrustedBuildTable = std::collections::HashMap<TrustedBuildKey, String>;

static TRUSTED_BUILDS: std::sync::OnceLock<std::sync::Mutex<TrustedBuildTable>> =
    std::sync::OnceLock::new();

fn trusted_builds() -> &'static std::sync::Mutex<TrustedBuildTable> {
    TRUSTED_BUILDS.get_or_init(|| std::sync::Mutex::new(TrustedBuildTable::new()))
}

/// 计算构建产物内容清单指纹（`<目录>/content-manifest.json` 整体 sha256）。
/// 宿主观察者登记受信构建与 install 核验暂存副本使用同一算法。
pub fn content_manifest_fingerprint(directory: &Path) -> Result<String> {
    let raw = std::fs::read(directory.join(crate::sidecar::CONTENT_MANIFEST_FILE)).with_context(
        || {
            format!(
                "读取构建产物内容清单失败: {}",
                directory
                    .join(crate::sidecar::CONTENT_MANIFEST_FILE)
                    .display()
            )
        },
    )?;
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(&raw)))
}

/// 登记一次成功构建的产物指纹（插件 × 项目 → 内容清单整体 sha256）；
/// `manifest_sha256` 为 None 表示撤销登记（构建失败或安装消费后失效）。
///
/// 指纹在观察者侧由「release 目录的 content-manifest.json 整体哈希」计算，
/// install 时与暂存副本逐一比对——授权对象是真实构建出的那份内容，
/// 构建后替换 release/ 无法通过签名安装。
pub fn note_trusted_build(plugin_id: &str, project_id: &str, manifest_sha256: Option<String>) {
    if let Ok(mut builds) = trusted_builds().lock() {
        match manifest_sha256 {
            Some(fingerprint) => {
                builds.insert((plugin_id.to_string(), project_id.to_string()), fingerprint);
            }
            None => {
                builds.remove(&(plugin_id.to_string(), project_id.to_string()));
            }
        }
    }
}

/// 读取项目当前登记的产物指纹（None 表示无有效登记）。
pub fn trusted_build_fingerprint(plugin_id: &str, project_id: &str) -> Option<String> {
    trusted_builds().lock().ok().and_then(|builds| {
        builds
            .get(&(plugin_id.to_string(), project_id.to_string()))
            .cloned()
    })
}

/// install 桥接入口的授权检查：调用方是宿主判定的受信创作插件，且目标
/// 项目存在受信构建登记。任一不满足即拒绝（fail-closed）。
fn ensure_install_authorized(
    storage_root: &Path,
    plugin_id: &str,
    project_id: &str,
) -> Result<String> {
    let official_signed = crate::registry::find_installed_plugin(storage_root, plugin_id)
        .ok()
        .and_then(|installed| installed.signed_release)
        .is_some_and(|release| release.publisher == crate::trust::OFFICIAL_PUBLISHER);
    let handler = TRUSTED_INSTALLER
        .read()
        .ok()
        .and_then(|current| current.clone())
        .ok_or_else(|| anyhow::anyhow!("宿主未接入受信安装方判定，拒绝签名安装（fail-closed）"))?;
    let identity = InstallerIdentity {
        plugin_id: plugin_id.to_string(),
        official_signed,
    };
    if !handler(&identity) {
        bail!("插件 {plugin_id} 无自动签名安装资格（用户密钥签名安装仅限受信创作插件）");
    }
    trusted_build_fingerprint(plugin_id, project_id).ok_or_else(|| {
        anyhow::anyhow!(
            "项目 {project_id} 缺少受信构建登记（先经该插件的 sidecar 完成构建，再安装）"
        )
    })
}

pub fn call(plugin_id: &str, method: &str, payload: &str) -> Result<String> {
    let install_dir = crate::registry::plugin_install_directory(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("plugin-dev 调用方插件 {plugin_id} 未加载"))?;
    let storage_root = storage_root_of(&install_dir)?;
    let operation = method.strip_prefix("plugin-dev.").unwrap_or_default();
    let result = match operation {
        "install" => {
            let request: IdRequest = parse_payload(payload)?;
            let expected_fingerprint =
                ensure_install_authorized(&storage_root, plugin_id, &request.id)?;
            let result = install(
                &storage_root,
                &request.id,
                Some((plugin_id, &expected_fingerprint)),
            )?;
            // 安装成功即消费登记：一次构建只授予一次安装资格，再次安装需
            // 重新构建（防止旧登记被无限复用）。
            note_trusted_build(plugin_id, &request.id, None);
            serde_json::to_value(result)?
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

/// 指纹核验期望：`(发起受信插件 ID, 登记的产物指纹)`。直调（测试/内部
/// 流程）传 None 跳过核验。
type TrustedBuildExpectation<'a> = Option<(&'a str, &'a str)>;

fn install(
    storage_root: &Path,
    project_id: &str,
    trusted_build: TrustedBuildExpectation<'_>,
) -> Result<InstallResult> {
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
    // 统一签名信任：创作链安装免交互——宿主以本机用户密钥为解释器插件
    // 自动签发签名清单（发布者 local），导入时经与官方/三方完全相同的
    // 签名验证路径（内容清单全树校验）。原生安装确认随之退役（信任根
    // 从「确认动作」转移到「用户密钥」，构建 → 签名 → 验证 → 安装全程
    // 可自动化，远程 / Agent 创作闭环不再被弹窗阻塞）。
    // 受信构建指纹核验（全部模板产物，含纯 UI / 工具 / sidecar 形态）：
    // 暂存副本的内容清单哈希必须与构建登记一致——授权对象是「真实构建
    // 出的那份内容」，构建后替换 release/（哪怕重算内容清单）都无法安装。
    // 直调（测试）传 None 跳过，生产路径必经 call 层携带登记指纹。
    if let Some((_installer, expected_fingerprint)) = trusted_build {
        let actual_fingerprint = content_manifest_fingerprint(staged.path())?;
        if !actual_fingerprint.eq_ignore_ascii_case(expected_fingerprint) {
            bail!(
                "项目 {project_id} 构建产物与受信构建登记不一致（构建后产物被修改？），\
                 请重新构建后再安装"
            );
        }
    }
    if manifest.sidecar.is_some() {
        sign_staged_with_user_key(storage_root, staged.path(), &manifest)?;
    }
    // 暂存/导入事务由安装链的 LOAD_OPERATION 全局锁串行化。
    let status = crate::registry::import_staged_plugin(storage_root, staged.path())?;
    tracing::info!(plugin = %status.id, version = %status.manifest_version, "plugin-dev 安装完成");
    Ok(InstallResult {
        plugin_id: status.id,
        version: status.manifest_version,
        state: status.state,
        enabled: status.enabled,
    })
}

/// 以用户密钥为暂存的解释器插件签发签名清单：构造 `SignedPluginRelease`
/// （发布者 local，锚定内容清单）并签名落盘。导入链的
/// `verify_signed_release` 会按 local 路由到用户公钥完成同款验证。
fn sign_staged_with_user_key(
    storage_root: &Path,
    staged_path: &Path,
    manifest: &PluginManifest,
) -> Result<()> {
    let manifest_path = staged_path.join(crate::manifest::MANIFEST_FILE);
    let content_manifest_path = staged_path.join(crate::sidecar::CONTENT_MANIFEST_FILE);
    // 签名前先双向校验内容清单（与旧本地信任落锚同款前置），不完整清单
    // 在安装时即拒绝。
    crate::sidecar::SidecarConfig::verify_integrity_manifest(&content_manifest_path, staged_path)?;
    let artifact = |path: &Path| -> Result<crate::signature::SignedArtifact> {
        Ok(crate::signature::SignedArtifact {
            path: path
                .strip_prefix(staged_path)
                .with_context(|| "签名制品路径推算失败")?
                .to_path_buf(),
            sha256: {
                use sha2::Digest;
                hex::encode(sha2::Sha256::digest(std::fs::read(path)?))
            },
        })
    };
    let release = crate::signature::SignedPluginRelease {
        schema_version: crate::signature::SIGNED_RELEASE_SCHEMA_VERSION,
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        publisher: crate::trust::LOCAL_PUBLISHER.to_string(),
        permissions: manifest.permissions.clone(),
        manifest: artifact(&manifest_path)?,
        wasm: None,
        ui: manifest
            .ui_contributions()
            .into_iter()
            .map(|contribution| artifact(&staged_path.join(contribution.entry)))
            .collect::<Result<Vec<_>>>()?,
        sidecar: None,
        content_manifest: Some(artifact(&content_manifest_path)?),
    };
    let release_path = staged_path.join(crate::signature::SIGNED_RELEASE_FILE);
    std::fs::write(&release_path, serde_json::to_vec_pretty(&release)?)?;
    crate::trust::sign_with_user_key(storage_root, &release_path)?;
    Ok(())
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

    /// 初始化测试配置并准备测试 Launcher 信任链：sidecar 恒走沙箱
    ///（策略表不接受关闭输入），本组测试以测试密钥签名的真实 Launcher
    /// 覆盖安装/签名/调用契约的完整链路。
    fn init_config_with_launcher(root: &Path) {
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        tiangong_config::registry::init_from_dir(&config_dir);
        crate::test_support::ensure_test_launcher_signed(root).unwrap();
    }

    fn wait_for_pid_file(path: &Path, timeout: std::time::Duration, context: &str) -> i32 {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(raw) = std::fs::read_to_string(path) {
                return raw
                    .trim()
                    .parse()
                    .unwrap_or_else(|error| panic!("{context} PID 无效: {error}"));
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{context} 未在 {timeout:?} 内生成 PID 文件：{}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

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
    #[serial_test::serial]
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

        let result = install(root.path(), "inst-demo", None).expect("安装完整链");
        assert_eq!(result.plugin_id, "inst-demo");
        assert_eq!(result.version, "9.9.9");
        assert!(result.enabled, "安装后应启用");
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
        let error = install(root.path(), "demo", None).unwrap_err();
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
    #[serial_test::serial]
    fn 解释器sidecar_创作链自动签名安装_真实调用与篡改拒绝() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "node-sc-demo";
        make_project(root.path(), id);
        make_node_sidecar_release(root.path(), id);

        let result = install(root.path(), id, None).expect("解释器 sidecar 安装");
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
    fn 解释器sidecar_无有效签名时拒绝启动() {
        let root = tempfile::tempdir().unwrap();
        let id = "node-sc-notrusted";
        make_project(root.path(), id);
        let release = make_node_sidecar_release(root.path(), id);
        // 手动把产物放进安装目录，不经 plugin-dev 确认链（无 local-trust.json）。
        let installed = root.path().join("plugins").join(id);
        copy_tree_for_test(&release, &installed);
        let error = crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({}))
            .expect_err("无有效签名应拒绝");
        assert!(
            format!("{error:#}").contains("需签名安装"),
            "应提示签名安装路径：{error:#}"
        );
    }

    /// 安装授权五场景：fail-closed、非受信插件、非官方签名的固定 ID、
    /// 缺构建登记、受信齐全。
    /// 调用方插件为已安装的 node demo 插件（install 本体直调装好）。
    #[test]
    #[serial_test::serial]
    fn 安装授权_五场景() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let caller = "node-sc-auth-caller";
        make_project(root.path(), caller);
        make_node_sidecar_release(root.path(), caller);
        // 待安装的项目产物。
        let target = "node-sc-auth-target";
        make_project(root.path(), target);
        make_node_sidecar_release(root.path(), target);
        // 调用方插件本体先经 install 直调装好（其自身安装不经桥接授权）。
        install(root.path(), caller, None).expect("调用方插件安装");

        let payload = |id: &str| serde_json::json!({ "id": id }).to_string();

        // 1) 未注入受信判定：fail-closed。
        clear_plugin_dev_trusted_installer_for_test();
        let error = call(caller, "plugin-dev.install", &payload(target))
            .expect_err("未注入受信判定应 fail-closed");
        assert!(format!("{error:#}").contains("fail-closed"), "{error:#}");

        // 2) 非受信插件（判定只认别的插件）。
        set_plugin_dev_trusted_installer(Arc::new(|identity: &InstallerIdentity| {
            identity.plugin_id == "someone-else"
        }));
        let error =
            call(caller, "plugin-dev.install", &payload(target)).expect_err("非受信插件应拒绝");
        assert!(
            format!("{error:#}").contains("无自动签名安装资格"),
            "{error:#}"
        );

        // 3) 受信但缺构建登记。
        let expected_caller = caller.to_string();
        set_plugin_dev_trusted_installer(Arc::new(move |identity: &InstallerIdentity| {
            identity.plugin_id == expected_caller
        }));
        let error =
            call(caller, "plugin-dev.install", &payload(target)).expect_err("缺构建登记应拒绝");
        assert!(
            format!("{error:#}").contains("缺少受信构建登记"),
            "{error:#}"
        );

        // 5) 受信 + 已登记（指纹锚定构建产物）：完整签名安装链成功。
        let target_fingerprint = {
            use sha2::Digest;
            let manifest_raw = std::fs::read(
                root.path()
                    .join(PLUGIN_DEV_DIR)
                    .join(target)
                    .join("release/content-manifest.json"),
            )
            .unwrap();
            hex::encode(sha2::Sha256::digest(&manifest_raw))
        };
        note_trusted_build(caller, target, Some(target_fingerprint.clone()));
        let result =
            call(caller, "plugin-dev.install", &payload(target)).expect("受信且已登记应安装成功");
        assert!(result.contains(target), "{result}");
        // 清理全局态，避免污染其他测试。
        clear_plugin_dev_trusted_installer_for_test();
    }

    /// 指纹核验：构建登记后篡改产物（重算内容清单）再经桥接安装必须拒绝；
    /// 成功安装消费登记（再次安装需重新构建）。
    #[test]
    #[serial_test::serial]
    fn 指纹核验_篡改产物拒绝与安装消费() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let caller = "node-sc-fp-caller";
        let target = "node-sc-fp-target";
        make_project(root.path(), caller);
        make_node_sidecar_release(root.path(), caller);
        make_project(root.path(), target);
        make_node_sidecar_release(root.path(), target);
        install(root.path(), caller, None).expect("调用方插件安装");

        let expected_caller = caller.to_string();
        set_plugin_dev_trusted_installer(Arc::new(move |identity: &InstallerIdentity| {
            identity.plugin_id == expected_caller
        }));
        let release_dir = root
            .path()
            .join(PLUGIN_DEV_DIR)
            .join(target)
            .join("release");
        let fingerprint = content_manifest_fingerprint(&release_dir).unwrap();
        note_trusted_build(caller, target, Some(fingerprint));

        // 构建后篡改产物（改文件并重算内容清单）：指纹失配拒绝。
        std::fs::write(
            release_dir.join("sidecar/main.mjs"),
            "// tampered after build",
        )
        .unwrap();
        write_content_manifest(&release_dir);
        let error = call(
            caller,
            "plugin-dev.install",
            &serde_json::json!({ "id": target }).to_string(),
        )
        .expect_err("篡改产物应拒绝安装");
        assert!(format!("{error:#}").contains("不一致"), "{error:#}");

        // 恢复产物并重新登记（等价重新构建）：安装成功且登记被消费。
        make_node_sidecar_release(root.path(), target);
        let fingerprint = content_manifest_fingerprint(&release_dir).unwrap();
        note_trusted_build(caller, target, Some(fingerprint));
        call(
            caller,
            "plugin-dev.install",
            &serde_json::json!({ "id": target }).to_string(),
        )
        .expect("登记匹配应安装成功");
        assert!(
            trusted_build_fingerprint(caller, target).is_none(),
            "安装成功后登记应被消费"
        );
        // 消费后再次安装：要求重新构建。
        let error = call(
            caller,
            "plugin-dev.install",
            &serde_json::json!({ "id": target }).to_string(),
        )
        .expect_err("消费后应要求重新构建");
        assert!(
            format!("{error:#}").contains("缺少受信构建登记"),
            "{error:#}"
        );
        clear_plugin_dev_trusted_installer_for_test();
    }

    /// 失败构建撤销：note(None) 语义清除旧登记后不可安装。
    #[test]
    #[serial_test::serial]
    fn 失败构建_撤销登记后不可安装() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let caller = "node-sc-revoke-caller";
        let target = "node-sc-revoke-target";
        make_project(root.path(), caller);
        make_node_sidecar_release(root.path(), caller);
        make_project(root.path(), target);
        make_node_sidecar_release(root.path(), target);
        install(root.path(), caller, None).expect("调用方插件安装");

        let expected_caller = caller.to_string();
        set_plugin_dev_trusted_installer(Arc::new(move |identity: &InstallerIdentity| {
            identity.plugin_id == expected_caller
        }));
        let release_dir = root
            .path()
            .join(PLUGIN_DEV_DIR)
            .join(target)
            .join("release");
        let fingerprint = content_manifest_fingerprint(&release_dir).unwrap();
        note_trusted_build(caller, target, Some(fingerprint));
        // 构建失败：观察者撤销登记。
        note_trusted_build(caller, target, None);
        let error = call(
            caller,
            "plugin-dev.install",
            &serde_json::json!({ "id": target }).to_string(),
        )
        .expect_err("撤销后应拒绝安装");
        assert!(
            format!("{error:#}").contains("缺少受信构建登记"),
            "{error:#}"
        );
        clear_plugin_dev_trusted_installer_for_test();
    }

    /// sidecar 结果观察者：bridge 的 sidecar.* 成功调用触发（宿主溯源登记
    /// 的机制基础）。
    #[test]
    #[serial_test::serial]
    fn sidecar观察者_成功调用触发() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "node-sc-observer";
        make_project(root.path(), id);
        make_node_sidecar_release(root.path(), id);
        install(root.path(), id, None).expect("安装");

        let observed = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let sink = Arc::clone(&observed);
        crate::bridge::set_sidecar_result_observer(Arc::new(
            move |plugin_id: &str, operation: &str, payload: &str, result: &str| {
                let mut records = sink.lock().unwrap();
                records.push((
                    format!("{plugin_id}|{operation}|{payload}|{result}"),
                    String::new(),
                ));
            },
        ));
        let response = crate::bridge_call(
            id,
            "sidecar.demo.echo",
            &serde_json::json!({"text": "obs"}).to_string(),
        )
        .expect("sidecar 桥接调用");
        assert!(response.contains("obs"), "{response}");
        let records = observed.lock().unwrap();
        assert!(
            records
                .iter()
                .any(|(record, _)| record.contains(&format!("{id}|demo.echo|"))),
            "观察者应收到成功调用记录：{records:?}"
        );
    }

    /// 三方导入链：签名归档 → 导入发布者公钥 → 暂存解包 → 签名验证安装
    /// → sidecar 真实调用 → 移除公钥后失效。
    #[test]
    #[serial_test::serial]
    fn 三方导入_签名归档安装与移除公钥后失效() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "node-sc-third-party";
        make_project(root.path(), id);
        let release = make_node_sidecar_release(root.path(), id);

        // 三方开发者视角：以自己的密钥为插件签发签名清单（发布者 acme-dev）。
        let sha256_of = |path: std::path::PathBuf| {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(std::fs::read(&path).unwrap()))
        };
        let release_json = json!({
            "schema_version": 1,
            "id": id,
            "version": "0.1.0",
            "publisher": "acme-dev",
            "permissions": ["sidecar.invoke"],
            "manifest": {
                "path": "plugin.json",
                "sha256": sha256_of(release.join("plugin.json")),
            },
            "ui": [{
                "path": "app/index.html",
                "sha256": sha256_of(release.join("app/index.html")),
            }],
            "content_manifest": {
                "path": "content-manifest.json",
                "sha256": sha256_of(release.join("content-manifest.json")),
            },
        });
        std::fs::write(
            release.join("release.json"),
            serde_json::to_vec_pretty(&release_json).unwrap(),
        )
        .unwrap();
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature = minisign::sign(
            Some(&keypair.pk),
            &keypair.sk,
            serde_json::to_vec_pretty(&release_json).unwrap().as_slice(),
            None,
            None,
        )
        .unwrap();
        use base64::Engine;
        std::fs::write(
            release.join("release.json.sig"),
            base64::engine::general_purpose::STANDARD.encode(signature.into_string()),
        )
        .unwrap();

        // 打三方分发归档（tar.zst，含签名清单与内容清单，排除 local-trust）。
        let archive = root.path().join(format!("{id}-0.1.0.tar.zst"));
        {
            let file = std::fs::File::create(&archive).unwrap();
            let encoder = zstd::Encoder::new(file, 3).unwrap();
            let mut builder = tar::Builder::new(encoder);
            let mut stack = vec![release.clone()];
            while let Some(directory) = stack.pop() {
                for entry in std::fs::read_dir(&directory).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    let name = entry.file_name();
                    if directory == release
                        && matches!(name.to_string_lossy().as_ref(), "local-trust.json")
                    {
                        continue;
                    }
                    if entry.file_type().unwrap().is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let relative = path.strip_prefix(&release).unwrap();
                    builder.append_path_with_name(&path, relative).unwrap();
                }
            }
            builder.into_inner().unwrap().finish().unwrap();
        }

        // 未导入公钥：归档可解包暂存，但安装（签名验证）拒绝。
        let staged =
            crate::artifacts::stage_plugin_archive(root.path(), &archive).expect("归档暂存");
        let error = crate::registry::import_staged_plugin(root.path(), staged.path())
            .expect_err("未导入三方公钥应拒绝安装");
        assert!(
            format!("{error:#}").contains("未导入"),
            "应给出公钥导入指引：{error:#}"
        );
        // 显式释放失败暂存（shadowing 的旧绑定活到作用域结束才 Drop，
        // 会干扰下方事务目录断言；生产路径失败返回即释放）。
        drop(staged);

        // 导入公钥后：安装 → 真实调用成功。
        let public_b64 = base64::engine::general_purpose::STANDARD
            .encode(keypair.pk.to_box().unwrap().into_string());
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &public_b64).unwrap();
        let staged =
            crate::artifacts::stage_plugin_archive(root.path(), &archive).expect("归档暂存");
        crate::registry::import_staged_plugin(root.path(), staged.path())
            .expect("三方签名插件安装");
        let response =
            crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({"text": "三方"}))
                .expect("三方插件 sidecar 调用");
        assert_eq!(response["text"], "三方");
        // 安装成功后事务目录不应累积残留（数据保留壳/坏残留即清）。
        let transactions = root.path().join("plugins").join(".transactions");
        let leftovers: Vec<_> = std::fs::read_dir(&transactions)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect();
        assert!(
            leftovers.is_empty(),
            "事务目录残留：{:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );

        // 移除公钥：下次启动即失效（重新发现验证时公钥缺失）。
        crate::trust::remove_trusted_publisher(root.path(), "acme-dev").unwrap();
        let error = crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({}))
            .expect_err("移除公钥后应拒绝启动");
        assert!(
            format!("{error:#}").contains("未导入"),
            "应报公钥缺失：{error:#}"
        );
    }

    /// 官方签名与本地信任混用拒绝：本地信任安装后再落入官方签名文件，
    /// 两种信任来源同时存在时启动门槛必须拒绝（来源不明确）。
    #[test]
    #[serial_test::serial]
    fn 解释器sidecar_官方签名与本地信任混用拒绝() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "node-sc-mixed-trust";
        make_project(root.path(), id);
        make_node_sidecar_release(root.path(), id);
        install(root.path(), id, None).expect("签名链安装");

        // 遗留本地信任插件（升级前存量形态）+ 官方签名同存：制造双信任
        // 来源。安装链已不落锚，这里手工补遗留标记。
        let installed = root.path().join("plugins").join(id);
        {
            use sha2::Digest;
            let manifest_raw = std::fs::read(installed.join("content-manifest.json")).unwrap();
            let anchor = hex::encode(sha2::Sha256::digest(&manifest_raw));
            std::fs::write(
                installed.join("local-trust.json"),
                format!(r#"{{"kind":"local-confirm","content_sha256":"{anchor}"}}"#),
            )
            .unwrap();
        }

        // 在已带遗留本地信任标记的安装目录上追加真实有效的签名
        // （解释器形态，内容清单哈希与目录一致）。

        let sha256_of = |path: std::path::PathBuf| {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(std::fs::read(&path).unwrap()))
        };
        let release = json!({
            "schema_version": 1,
            "id": id,
            "version": "0.1.0",
            "publisher": "acme-dev",
            "permissions": ["sidecar.invoke"],
            "manifest": {
                "path": "plugin.json",
                "sha256": sha256_of(installed.join("plugin.json")),
            },
            "ui": [{
                "path": "app/index.html",
                "sha256": sha256_of(installed.join("app/index.html")),
            }],
            "content_manifest": {
                "path": "content-manifest.json",
                "sha256": sha256_of(installed.join("content-manifest.json")),
            },
        });
        let release_raw = serde_json::to_vec_pretty(&release).unwrap();
        std::fs::write(installed.join("release.json"), &release_raw).unwrap();
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature = minisign::sign(
            Some(&keypair.pk),
            &keypair.sk,
            release_raw.as_slice(),
            None,
            None,
        )
        .unwrap();
        // 与 tauri signer 输出一致：两行签名文本整体 base64（verify_minisign 格式）。
        use base64::Engine;
        std::fs::write(
            installed.join("release.json.sig"),
            base64::engine::general_purpose::STANDARD.encode(signature.into_string()),
        )
        .unwrap();
        // 三方发布者公钥经登记表导入（官方信任根不可配置，不适用于本测试）。
        let pubkey_b64 = base64::engine::general_purpose::STANDARD
            .encode(keypair.pk.to_box().unwrap().into_string());
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &pubkey_b64).unwrap();
        let error = crate::registry::invoke_sidecar(root.path(), id, "demo.echo", json!({}))
            .expect_err("混用信任来源应拒绝启动");
        assert!(
            format!("{error:#}").contains("同时携带官方签名与本地信任标记"),
            "应报混用拒绝：{error:#}"
        );
    }

    /// 真实 creator 产物全链路：package → plugin-dev 安装（原生确认 + 暂存 +
    /// 双向完整性 + 本地信任落锚）→ 按需 sidecar 真实执行 devkit.init（验证
    /// templates 随行资源经 resources 声明进入安装目录并被 devkit 使用）。
    /// 产物（release/）由 `yarn package` 生成、不入库：CI 无产物时跳过。
    #[test]
    #[serial_test::serial]
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
        init_config_with_launcher(root.path());
        let id = "plugin-creator";
        let dev_root = root.path().join(PLUGIN_DEV_DIR).join(id);
        copy_tree_for_test(&release_source, &dev_root.join("release"));
        make_project(root.path(), id);

        let result = install(root.path(), id, None).expect("creator 真实产物安装");
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
    /// → 注入自定义操作 → devkit 真实构建（yarn 工程链）→ 自动签名安装 →
    /// 宿主连接新插件的 sidecar 真实调用。全程不经 GUI，等价于 Agent 操作序列。
    #[test]
    #[ignore = "真实 yarn 构建需数分钟与网络，按需显式运行"]
    #[serial_test::serial]
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
        init_config_with_launcher(root.path());
        let creator = "plugin-creator";
        let dev_root = root.path().join(PLUGIN_DEV_DIR).join(creator);
        copy_tree_for_test(&release_source, &dev_root.join("release"));
        make_project(root.path(), creator);
        install(root.path(), creator, None).expect("creator 安装");

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
        let result = install(root.path(), project_id, None).expect("新插件安装");
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
    /// 与任何真实指纹都不可能相等的占位（拒绝路径用）。
    fn unreachable_fingerprint() -> String {
        "0".repeat(64)
    }

    /// 纯 UI 插件的构建指纹核验（无 sidecar 形态同样受登记约束）：构建后
    /// 修改产物被拒，重新登记（等价重新构建）后放行。
    #[test]
    #[serial_test::serial]
    fn 纯ui插件_构建指纹核验() {
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "ui-fp-demo";
        make_project(root.path(), id);
        // 纯 UI 产物：ui 贡献 + 内容清单，无 sidecar（devkit 全模板统一生成清单）。
        let release = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("app")).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            format!(
                r#"{{"schema_version":2,"id":"{id}","version":"0.1.0","permissions":[],"ui":{{"contributions":[{{"slot":"extension.tab","id":"app","entry":"app/index.html"}}]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(release.join("app/index.html"), "<html></html>").unwrap();
        write_content_manifest(&release);

        // 受信调用方（模拟宿主判定）+ 指纹登记后经桥接安装成功。
        let fingerprint = content_manifest_fingerprint(&release).unwrap();
        note_trusted_build("plugin-creator", id, Some(fingerprint));
        // 桥接调用要求调用方插件已加载——本测试聚焦指纹路径，直调 install
        // 并显式传入登记期望（与 call 层同款核验分支）。
        let install_result = install(
            root.path(),
            id,
            Some(("plugin-creator", &unreachable_fingerprint())),
        );
        // 上面故意用失配指纹断言拒绝路径：
        assert!(
            install_result.is_err_and(|error| format!("{error:#}").contains("不一致")),
            "纯 UI 产物同样受指纹核验约束"
        );
        // 正确指纹放行。
        let release_dir = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        let fingerprint = content_manifest_fingerprint(&release_dir).unwrap();
        let result = install(root.path(), id, Some(("plugin-creator", &fingerprint)))
            .expect("纯 UI 指纹匹配应安装成功");
        assert_eq!(result.plugin_id, id);
    }

    /// 无界面纯工具插件（node-tool 形态）：无 ui 贡献即可安装（校验解耦），
    /// 工具契约（操作名=工具名、ToolOutcome 形状）经 sidecar 真实往返。
    #[test]
    #[serial_test::serial]
    fn 无界面纯工具插件_安装与工具契约() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "pure-tool-demo";
        make_project(root.path(), id);
        // 构造 node-tool 形态产物：tools + sidecar，无 ui/wasm。
        let release = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("sidecar/vendor/tiangong-sidecar-sdk")).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            r#"{"schema_version":2,"id":"pure-tool-demo","version":"0.1.0","entrypoints":["desktop"],"permissions":["tool.provide","sidecar.invoke"],"capabilities":{"tools":true},"tools":[{"name":"text_analyze","description":"文本分析","input_schema":{"type":"object"},"timeout_ms":20000}],"sidecar":{"runtime":"node","entry":"sidecar/main.mjs"},"mention":{"hint":"纯工具"}}"#,
        )
        .unwrap();
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
  pluginId: 'pure-tool-demo',
  dispatch(operation, payload) {
    if (operation === 'text_analyze') {
      const text = typeof payload?.text === 'string' ? payload.text : '';
      return { payload: { ok: true, summary: `文本 ${[...text].length} 字`, stdout: [...text].reverse().join(''), stderr: '', exit_code: 0 } };
    }
    return { payload: {} };
  },
});
"#,
        )
        .unwrap();
        write_content_manifest(&release);

        let result = install(root.path(), id, None).expect("无界面纯工具插件应可安装");
        assert_eq!(result.plugin_id, id);

        // 工具契约：操作名 = 工具名，参数 = 工具参数对象，返回 ToolOutcome。
        let response = crate::registry::invoke_sidecar(
            root.path(),
            id,
            "text_analyze",
            json!({"text": "天工abc"}),
        )
        .expect("工具直连调用");
        assert_eq!(response["ok"], json!(true), "响应: {response}");
        assert_eq!(response["summary"], json!("文本 5 字"));
        assert_eq!(response["stdout"], json!("cba工天"));
    }
    /// 自定义图标往返：带 png 图标的插件安装后，read_plugin_icon 返回正确
    /// 字节与 MIME（read 走 loaded_plugins 内存表，install 后可用）。
    #[test]
    #[serial_test::serial]
    fn 插件图标_安装与读取往返() {
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "icon-demo";
        make_project(root.path(), id);
        let release = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("app")).unwrap();
        std::fs::create_dir_all(release.join("icons")).unwrap();
        // 最小 PNG（1x1 透明像素）。
        const MINIMAL_PNG: [u8; 67] = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(release.join("icons/app.png"), MINIMAL_PNG).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            r#"{"schema_version":2,"id":"icon-demo","version":"0.1.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","title":"图标验证","entry":"app/index.html","icon":"icons/app.png"}]}}"#,
        )
        .unwrap();
        std::fs::write(release.join("app/index.html"), "<html></html>").unwrap();
        write_content_manifest(&release);

        install(root.path(), id, None).expect("带图标插件安装");

        let (data, mime) = crate::registry::read_plugin_icon(id, "app").expect("读取插件图标");
        assert_eq!(mime, "image/png");
        assert_eq!(data.as_slice(), MINIMAL_PNG);
    }
    /// 工具级超时与进程终止：阻塞型 node 工具（sleep）超过工具声明的
    /// timeout_ms 时及时失败，且按需 sidecar 进程被终止（不留活进程）。
    #[test]
    #[serial_test::serial]
    fn 直连工具_超时终止进程() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "blocking-tool";
        make_project(root.path(), id);
        let release = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("sidecar/vendor/tiangong-sidecar-sdk")).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            r#"{"schema_version":2,"id":"blocking-tool","version":"0.1.0","entrypoints":["desktop"],"permissions":["tool.provide","sidecar.invoke"],"capabilities":{"tools":true},"tools":[{"name":"slow_job","description":"慢任务","input_schema":{"type":"object"},"timeout_ms":30000}],"sidecar":{"runtime":"node","entry":"sidecar/main.mjs","request_timeout_ms":60000}}"#,
        )
        .unwrap();
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
import { writeFileSync } from 'node:fs';
import { runSidecar } from './vendor/tiangong-sidecar-sdk/index.mjs';
await runSidecar({
  pluginId: 'blocking-tool',
  dispatch(operation, payload) {
    if (operation === 'slow_job') {
      writeFileSync(payload.pid_file, String(process.pid));
      return new Promise(() => {});
    }
    return { payload: {} };
  },
});
"#,
        )
        .unwrap();
        write_content_manifest(&release);

        install(root.path(), id, None).expect("安装阻塞工具插件");

        let pid_file = root.path().join("slow.pid");
        let started = std::time::Instant::now();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                crate::ts_plugin::invoke_sidecar_tool_for_test(
                    id,
                    tiangong_llm::tool::ToolCall {
                        id: "test-call".to_string(),
                        name: "slow_job".to_string(),
                        arguments: json!({"pid_file": &pid_file}),
                    },
                    30000,
                )
                .await
            });
        assert!(
            // 沙箱冷启动（Launcher 验签+隔离层+解释器）计入工具预算。
            started.elapsed() < std::time::Duration::from_secs(45),
            "超时应及时返回，实际 {:?}",
            started.elapsed()
        );
        assert!(!result.ok, "超时应失败: {}", result.summary);
        // 进程终止断言：pid 文件出现后，对应进程应在短时间内消失。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let pid = wait_for_pid_file(
            &pid_file,
            std::time::Duration::from_secs(5),
            &format!("慢调用结果：{}", result.summary),
        );
        while std::time::Instant::now() < deadline {
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let alive = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "超时后按需 sidecar 进程（pid {pid}）应被终止");
    }
    /// 并发隔离：同一插件并发直连调用互不影响——两个并发快调用都成功；
    /// 一个慢调用超时被终止的同时，并发进行的快调用正常完成；结束后无
    /// 遗留进程。
    #[test]
    #[serial_test::serial]
    fn 直连工具_并发隔离与取消归属() {
        let Some(_node) = find_node_for_test() else {
            eprintln!("跳过：PATH 中未找到 node");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        init_config_with_launcher(root.path());
        let id = "concurrent-tool";
        make_project(root.path(), id);
        let release = root.path().join(PLUGIN_DEV_DIR).join(id).join("release");
        std::fs::create_dir_all(release.join("sidecar/vendor/tiangong-sidecar-sdk")).unwrap();
        std::fs::write(
            release.join("plugin.json"),
            r#"{"schema_version":2,"id":"concurrent-tool","version":"0.1.0","entrypoints":["desktop"],"permissions":["tool.provide","sidecar.invoke"],"capabilities":{"tools":true},"tools":[{"name":"quick_job","description":"快","input_schema":{"type":"object"},"timeout_ms":60000},{"name":"slow_job","description":"慢","input_schema":{"type":"object"},"timeout_ms":30000}],"sidecar":{"runtime":"node","entry":"sidecar/main.mjs","request_timeout_ms":60000}}"#,
        )
        .unwrap();
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
import { writeFileSync } from 'node:fs';
import { runSidecar } from './vendor/tiangong-sidecar-sdk/index.mjs';
await runSidecar({
  pluginId: 'concurrent-tool',
  dispatch(operation, payload) {
    if (operation === 'quick_job') {
      return { payload: { ok: true, summary: `完成 ${payload?.tag ?? ''}`, stdout: '', stderr: '', exit_code: 0 } };
    }
    if (operation === 'slow_job') {
      writeFileSync(payload.pid_file, String(process.pid));
      return new Promise(() => {});
    }
    return { payload: {} };
  },
});
"#,
        )
        .unwrap();
        write_content_manifest(&release);
        install(root.path(), id, None).expect("安装并发工具插件");

        let pid_file = root.path().join("slow.pid");
        let call = |name: &str, args: serde_json::Value| tiangong_llm::tool::ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments: args,
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::join!(
                    async {
                        let result = crate::ts_plugin::invoke_sidecar_tool_for_test(
                            id,
                            call("quick_job", json!({"tag": "并发一"})),
                            60000,
                        )
                        .await;
                        assert!(result.ok, "并发快调用一应成功: {}", result.summary);
                        assert_eq!(result.summary, "完成 并发一");
                    },
                    async {
                        let result = crate::ts_plugin::invoke_sidecar_tool_for_test(
                            id,
                            call("quick_job", json!({"tag": "并发二"})),
                            60000,
                        )
                        .await;
                        assert!(result.ok, "并发快调用二应成功: {}", result.summary);
                        assert_eq!(result.summary, "完成 并发二");
                    },
                    async {
                        let result = crate::ts_plugin::invoke_sidecar_tool_for_test(
                            id,
                            call("slow_job", json!({"pid_file": &pid_file})),
                            30000,
                        )
                        .await;
                        assert!(!result.ok, "慢调用应超时失败");
                    },
                );
            });

        // 慢调用的按需进程被终止；无遗留。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let pid = wait_for_pid_file(&pid_file, std::time::Duration::from_secs(5), "并发慢调用");
        let process_alive = |pid: i32| {
            std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        while process_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!process_alive(pid), "超时调用的进程（pid {pid}）应被终止");
    }
}
