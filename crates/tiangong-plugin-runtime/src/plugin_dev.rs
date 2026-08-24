//! plugin-dev 受限桥接服务（RFC 0017 §11 plugin creator 的宿主承载）。
//!
//! plugin creator 类插件运行在 webview 沙箱内，无法 spawn 进程或直接写文件
//! 系统；本模块按 RFC D23 提供专用受限通道：写范围锁定开发目录
//! `<storage_root>/plugins-dev/<项目id>/`，日志只读，不可触达信任库、公钥库
//! 与宿主设置。服务保持插件中立——模板源取自调用方插件自身安装目录下的
//! `templates/`，任何声明 `plugin-dev.use` 权限的插件均可复用本通道。
//!
//! 安装必须经宿主原生确认对话框（非 webview，Agent 的界面自动化无法触达），
//! confirm 回调由桌面入口注入；未注入时 fail-closed 拒绝安装。

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::manifest::PluginManifest;

/// 开发目录名（位于存储根下，与 plugins/ 平级）。
pub const PLUGIN_DEV_DIR: &str = "plugins-dev";
/// 模板源目录名（位于调用方插件安装目录下）。
const TEMPLATE_SOURCE_DIR: &str = "templates";
/// 项目元数据文件名（位于项目目录根部）。
const PROJECT_META_FILE: &str = ".plugin-dev.json";
/// 构建日志文件名（位于项目 logs/ 下）。
const BUILD_LOG_FILE: &str = "build.log";
/// 构建总时长上限（yarn install + build + package）。
const BUILD_TIMEOUT: Duration = Duration::from_secs(240);
/// 日志读取时最多回看的文件尾部字节数。
const LOG_TAIL_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 生成插件的最低 Node 大版本（vite 构建链要求）。
const MIN_NODE_MAJOR: u32 = 20;

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

/// 同一项目的构建互斥（plugin-dev.build 并发防护）。
static BUILD_LOCKS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// 处理一次 `plugin-dev.*` 桥接调用（权限校验由 bridge 层完成）。
pub fn call(plugin_id: &str, method: &str, payload: &str) -> Result<String> {
    let install_dir = crate::registry::plugin_install_directory(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("plugin-dev 调用方插件 {plugin_id} 未加载"))?;
    let storage_root = storage_root_of(&install_dir)?;
    let operation = method.strip_prefix("plugin-dev.").unwrap_or_default();
    let result = match operation {
        "init" => {
            let request: InitRequest = parse_payload(payload)?;
            serde_json::to_value(init(&storage_root, &install_dir, &request)?)?
        }
        "list" => serde_json::to_value(list(&storage_root)?)?,
        "validate" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(validate(&storage_root, &request.id)?)?
        }
        "build" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(build(&storage_root, &request.id)?)?
        }
        "install" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(install(&storage_root, &request.id)?)?
        }
        "logs" => {
            let request: LogsRequest = parse_payload(payload)?;
            serde_json::to_value(logs(&storage_root, &request)?)?
        }
        "status" => {
            let request: IdRequest = parse_payload(payload)?;
            serde_json::to_value(status(&storage_root, &request.id)?)?
        }
        _ => bail!(
            "plugin-dev 未知方法 {method}（可用：init/list/validate/build/install/logs/status）"
        ),
    };
    Ok(result.to_string())
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
struct InitRequest {
    template: String,
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdRequest {
    id: String,
}

#[derive(Debug, Deserialize)]
struct LogsRequest {
    /// `dev:<项目id>` 读构建日志；`plugin:<插件id>` 读已安装插件运行日志。
    target: String,
    #[serde(default = "default_log_lines")]
    lines: usize,
}

fn default_log_lines() -> usize {
    100
}

#[derive(Debug, Serialize)]
struct InitResult {
    plugin_id: String,
    name: String,
    template: String,
    directory: String,
    files: usize,
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
struct ValidateResult {
    ok: bool,
    /// 阻断性问题（清单非法等，必须修复才能构建/安装）。
    errors: Vec<String>,
    /// 非阻断提示（如尚未构建、制品缺失）。
    warnings: Vec<String>,
    id: Option<String>,
    version: Option<String>,
    permissions: Vec<String>,
    tools: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BuildResult {
    duration_ms: u128,
    /// 日志尾部（最近 4KB），完整日志见项目 logs/build.log。
    log_tail: String,
    release_dir: String,
}

#[derive(Debug, Serialize)]
struct InstallResult {
    plugin_id: String,
    version: String,
    state: String,
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct LogsResult {
    path: String,
    lines: Vec<String>,
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

// ── init ──

fn init(
    storage_root: &Path,
    caller_install_dir: &Path,
    request: &InitRequest,
) -> Result<InitResult> {
    validate_project_id(&request.id)?;
    // 防劫持：不得与已安装插件同名顶替；防自举：不得与调用方自身同名。
    if installed_plugin_manifest(storage_root, &request.id).is_some() {
        bail!("插件 ID {} 已被已安装插件占用，请更换 ID", request.id);
    }
    if request.id == caller_plugin_id(caller_install_dir) {
        bail!(
            "插件 ID {} 与 plugin-dev 服务调用方自身相同（防自举），请更换 ID",
            request.id
        );
    }
    // 模板名只允许简单目录名，防路径注入。
    if request
        .template
        .bytes()
        .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-' && byte != b'_')
    {
        bail!("模板名只能是 ASCII 字母数字与 - _：{}", request.template);
    }
    let template_dir = caller_install_dir
        .join(TEMPLATE_SOURCE_DIR)
        .join(&request.template);
    if !template_dir.is_dir() {
        let available = list_templates(caller_install_dir).join("、");
        bail!(
            "模板 {} 不存在（调用方插件目录 {TEMPLATE_SOURCE_DIR}/ 下可用模板：{available}）",
            request.template
        );
    }
    let project_dir = dev_project_dir(storage_root, &request.id)?;
    if project_dir.exists() {
        bail!(
            "开发项目 {} 已存在：{}（迭代请直接编辑该项目，勿重复 init）",
            request.id,
            project_dir.display()
        );
    }
    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("创建开发目录失败: {}", project_dir.display()))?;
    let name = request.name.clone().unwrap_or_else(|| request.id.clone());
    let files = copy_template_with_placeholders(&template_dir, &project_dir, &request.id, &name)?;
    let meta = json!({
        "plugin_id": request.id,
        "name": name,
        "template": request.template,
        "created_at": chrono::Local::now().naive_local().format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    std::fs::write(
        project_dir.join(PROJECT_META_FILE),
        serde_json::to_vec_pretty(&meta)?,
    )
    .with_context(|| format!("写入项目元数据失败: {}", project_dir.display()))?;
    tracing::info!(plugin = %request.id, template = %request.template, "plugin-dev 项目初始化完成");
    Ok(InitResult {
        plugin_id: request.id.clone(),
        name,
        template: request.template.clone(),
        directory: project_dir.display().to_string(),
        files,
    })
}

fn caller_plugin_id(caller_install_dir: &Path) -> String {
    caller_install_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn list_templates(caller_install_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(caller_install_dir.join(TEMPLATE_SOURCE_DIR)) else {
        return names;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// 复制模板并替换占位符（文本文件替换 `{{PLUGIN_ID}}`/`{{PLUGIN_NAME}}`，二进制原样）。
fn copy_template_with_placeholders(
    source: &Path,
    destination: &Path,
    plugin_id: &str,
    name: &str,
) -> Result<usize> {
    const TEXT_EXTENSIONS: &[&str] = &[
        "json", "html", "ts", "tsx", "vue", "js", "mjs", "cjs", "css", "md", "txt", "yml", "yaml",
    ];
    let mut files = 0usize;
    let mut stack = vec![source.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("读取模板目录失败: {}", directory.display()))?
        {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let entry_path = entry.path();
            if metadata.file_type().is_symlink() {
                bail!("模板路径不能包含符号链接: {}", entry_path.display());
            }
            let relative = entry_path.strip_prefix(source)?;
            let target = destination.join(relative);
            if metadata.is_dir() {
                std::fs::create_dir_all(&target)
                    .with_context(|| format!("创建项目目录失败: {}", target.display()))?;
                stack.push(entry_path);
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let is_text = entry_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| TEXT_EXTENSIONS.contains(&ext));
                if is_text {
                    let content = std::fs::read_to_string(&entry_path)
                        .with_context(|| format!("读取模板文件失败: {}", entry_path.display()))?;
                    let content = content
                        .replace("{{PLUGIN_ID}}", plugin_id)
                        .replace("{{PLUGIN_NAME}}", name);
                    std::fs::write(&target, content)
                        .with_context(|| format!("写入项目文件失败: {}", target.display()))?;
                } else {
                    std::fs::copy(&entry_path, &target).with_context(|| {
                        format!(
                            "复制模板文件失败: {} -> {}",
                            entry_path.display(),
                            target.display()
                        )
                    })?;
                }
                files += 1;
            }
        }
    }
    Ok(files)
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
                "读取项目元数据失败: {}",
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

// ── validate ──

fn validate(storage_root: &Path, project_id: &str) -> Result<ValidateResult> {
    let project_dir = dev_project_dir(storage_root, project_id)?;
    let manifest_path = project_dir.join("plugin.json");
    if !manifest_path.is_file() {
        return Ok(ValidateResult {
            ok: false,
            errors: vec![format!("项目 {project_id} 缺少 plugin.json")],
            warnings: vec![],
            id: None,
            version: None,
            permissions: vec![],
            tools: vec![],
        });
    }
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let manifest = match PluginManifest::load(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return Ok(ValidateResult {
                ok: false,
                errors: vec![format!("plugin.json 校验失败：{error:#}")],
                warnings,
                id: None,
                version: None,
                permissions: vec![],
                tools: vec![],
            });
        }
    };
    // UI 入口存在性：源码态允许尚未构建（warning），但路径非法视为 error。
    for contribution in manifest.ui_contributions() {
        let entry = project_dir.join(Path::new(&contribution.entry));
        match entry.try_exists() {
            Ok(true) => {}
            Ok(false) => warnings.push(format!(
                "UI 入口 {} 尚不存在（通常表示还未执行构建）",
                contribution.entry
            )),
            Err(error) => errors.push(format!("UI 入口 {} 路径异常：{error}", contribution.entry)),
        }
    }
    if manifest.ui_contributions().is_empty()
        && manifest.tools.is_none()
        && manifest.wasm_binary().is_none()
    {
        warnings.push("清单未声明任何 UI 贡献、工具或逻辑层，安装后不会有可见效果".to_string());
    }
    Ok(ValidateResult {
        ok: errors.is_empty(),
        errors,
        warnings,
        id: Some(manifest.id),
        version: Some(manifest.version),
        permissions: manifest.permissions,
        tools: manifest
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|tool| tool.name)
            .collect(),
    })
}

// ── build ──

fn build(storage_root: &Path, project_id: &str) -> Result<BuildResult> {
    let project_dir = dev_project_dir(storage_root, project_id)?;
    if !project_dir.join("plugin.json").is_file() {
        bail!("项目 {project_id} 不存在或缺少 plugin.json（先执行 init）");
    }
    {
        let mut locks = BUILD_LOCKS.lock().expect("plugin-dev 构建锁中毒");
        if !locks.insert(project_id.to_string()) {
            bail!("项目 {project_id} 正在构建中，请等待完成后再试");
        }
    }
    let started = Instant::now();
    let result = run_build_steps(&project_dir);
    let _ = BUILD_LOCKS
        .lock()
        .expect("plugin-dev 构建锁中毒")
        .remove(project_id);
    let duration_ms = started.elapsed().as_millis();
    let log_tail = read_log_tail(&project_dir.join("logs").join(BUILD_LOG_FILE), 4096);
    match result {
        Ok(()) => {
            let release_dir = project_dir.join("release");
            tracing::info!(plugin = project_id, "plugin-dev 构建完成");
            Ok(BuildResult {
                duration_ms,
                log_tail,
                release_dir: release_dir.display().to_string(),
            })
        }
        Err(error) => {
            let message = error.to_string();
            // 构建失败以携带日志尾部的错误回传，便于 Agent 与创作页自诊断。
            Err(anyhow::anyhow!(
                "构建失败（{message}）。完整日志：{}，可经 plugin_logs 读取。日志尾部：\n{log_tail}",
                project_dir.join("logs").join(BUILD_LOG_FILE).display()
            ))
        }
    }
}

/// 构建入口：无 package.json 的项目（零构建模板，如 ui-app）走宿主内建
/// 打包；有 package.json 的工程走 yarn install → build → package。
fn run_build_steps(project_dir: &Path) -> Result<()> {
    let logs_dir = project_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join(BUILD_LOG_FILE);
    let mut log = std::fs::File::create(&log_path)
        .with_context(|| format!("创建构建日志失败: {}", log_path.display()))?;
    writeln!(
        log,
        "# 构建开始 {}",
        chrono::Local::now()
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S")
    )?;
    if project_dir.join("package.json").is_file() {
        run_yarn_build(project_dir, &mut log)
    } else {
        run_zero_build(project_dir, &mut log)
    }?;
    writeln!(
        log,
        "\n# 构建完成 {}",
        chrono::Local::now()
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S")
    )?;
    Ok(())
}

/// 零构建打包：复制 plugin.json 与各 UI 入口所在顶层目录到 release/，
/// 并生成内容树清单（路径 + sha256）。不依赖 Node 工具链。
fn run_zero_build(project_dir: &Path, log: &mut std::fs::File) -> Result<()> {
    writeln!(log, "$ zero-build package（零构建模板，无需 Node）")?;
    let manifest = PluginManifest::load(&project_dir.join("plugin.json"))
        .context("零构建打包前清单校验失败")?;
    let release_dir = project_dir.join("release");
    if release_dir.exists() {
        std::fs::remove_dir_all(&release_dir)
            .with_context(|| format!("清理旧产物失败: {}", release_dir.display()))?;
    }
    std::fs::create_dir_all(&release_dir)?;
    std::fs::copy(
        project_dir.join("plugin.json"),
        release_dir.join("plugin.json"),
    )
    .context("复制 plugin.json 到 release 失败")?;
    // 复制每个 UI 入口所在的顶层目录（如 app/）；入口无目录前缀
    // （如 index.html）时按单文件复制。
    let mut top_dirs: BTreeSet<String> = BTreeSet::new();
    let mut single_files: Vec<String> = Vec::new();
    for contribution in manifest.ui_contributions() {
        let entry = Path::new(&contribution.entry);
        if entry.components().count() > 1 {
            if let Some(first) = entry.components().next() {
                if let Some(name) = first.as_os_str().to_str() {
                    top_dirs.insert(name.to_string());
                }
            }
        } else {
            single_files.push(contribution.entry.clone());
        }
    }
    for directory in &top_dirs {
        let source = project_dir.join(directory);
        if !source.is_dir() {
            continue;
        }
        copy_tree(&source, &release_dir.join(directory))
            .with_context(|| format!("复制零构建资产目录 {directory} 失败"))?;
        writeln!(log, "# 复制 {directory}/")?;
    }
    for file in &single_files {
        let source = project_dir.join(file);
        let destination = release_dir.join(file);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, &destination)
            .with_context(|| format!("复制零构建入口文件 {file} 失败"))?;
        writeln!(log, "# 复制 {file}")?;
    }
    write_content_manifest(&release_dir)?;
    writeln!(log, "# 内容树清单已生成")?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.file_type().is_symlink() {
            bail!("零构建资产不能包含符号链接: {}", entry.path().display());
        }
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 生成内容树清单 release/content-manifest.json（路径 + sha256 逐条，
/// 供内容哈希锁定直接消费；不含清单自身）。
fn write_content_manifest(release_dir: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<(String, String)> = Vec::new();
    let mut stack = vec![release_dir.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(release_dir)?
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "content-manifest.json" {
                continue;
            }
            let digest = Sha256::digest(std::fs::read(&path)?);
            files.push((relative, hex::encode(digest)));
        }
    }
    files.sort();
    let manifest = serde_json::json!({
        "algorithm": "sha256",
        "files": files
            .into_iter()
            .map(|(path, sha256)| serde_json::json!({ "path": path, "sha256": sha256 }))
            .collect::<Vec<_>>(),
    });
    std::fs::write(
        release_dir.join("content-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

/// 顺序执行 yarn install → yarn build → yarn package，输出实时落 logs/build.log。
fn run_yarn_build(project_dir: &Path, log: &mut std::fs::File) -> Result<()> {
    ensure_toolchain(log)?;
    for (step, args) in [
        ("install", vec!["install", "--silent"]),
        ("build", vec!["run", "build"]),
        ("package", vec!["run", "package"]),
    ] {
        writeln!(log, "\n$ yarn {}", args.join(" "))?;
        log.flush()?;
        let output = run_yarn(project_dir, &args, BUILD_TIMEOUT)?;
        write!(log, "{}", String::from_utf8_lossy(&output.stdout))?;
        write!(log, "{}", String::from_utf8_lossy(&output.stderr))?;
        if !output.status.success() {
            writeln!(
                log,
                "\n# 步骤 [{step}] 退出码 {}",
                output.status.code().unwrap_or(-1)
            )?;
            bail!(
                "步骤 [{step}] 失败（退出码 {:?}），详见构建日志",
                output.status.code()
            );
        }
        writeln!(log, "# 步骤 [{step}] 完成")?;
        log.flush()?;
    }
    writeln!(
        log,
        "\n# 构建完成 {}",
        chrono::Local::now()
            .naive_local()
            .format("%Y-%m-%d %H:%M:%S")
    )?;
    Ok(())
}

/// 探测 node/yarn 与 Node 版本门槛；GUI 应用 PATH 常缺 Homebrew 等位置，探测时补充。
fn ensure_toolchain(log: &mut std::fs::File) -> Result<()> {
    writeln!(log, "$ node --version")?;
    let node = run_yarn_probe("node", &["--version"])?;
    let version = String::from_utf8_lossy(&node.stdout).trim().to_string();
    writeln!(log, "{version}")?;
    let major = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .context("无法解析 node 版本输出")?;
    if major < MIN_NODE_MAJOR {
        bail!("Node 版本过低（当前 v{major}.x，要求 ≥ {MIN_NODE_MAJOR}），请升级 Node 后重试");
    }
    writeln!(log, "$ yarn --version")?;
    let yarn = run_yarn_probe("yarn", &["--version"])?;
    writeln!(log, "{}", String::from_utf8_lossy(&yarn.stdout).trim())?;
    Ok(())
}

/// 构建子进程环境：在 GUI 继承的 PATH 基础上补充常见包管理器安装位置。
fn build_command(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    let mut path = std::env::var("PATH").unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        if Path::new(extra).exists() && !path.split(':').any(|part| part == extra) {
            path.push(':');
            path.push_str(extra);
        }
    }
    command.env("PATH", path);
    command
}

fn run_yarn_probe(program: &str, args: &[&str]) -> Result<std::process::Output> {
    build_command(program, args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            anyhow::anyhow!(
                "未找到 {program}（{error}）。插件构建需要 Node ≥ {MIN_NODE_MAJOR} 与 yarn，\
                 请安装后重试：https://nodejs.org 与 https://yarnpkg.com"
            )
        })
}

/// 执行一条 yarn 命令：捕获输出并遵守总超时（超时终止进程树尽力而为）。
fn run_yarn(cwd: &Path, args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    let mut command = build_command("yarn", args);
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        anyhow::anyhow!("未找到 yarn（{error}）。请安装 yarn 后重试：https://yarnpkg.com")
    })?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                #[cfg(unix)]
                {
                    let pgid = child.id() as i32;
                    unsafe {
                        libc::kill(-pgid, libc::SIGKILL);
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = child.kill();
                }
                let _ = child.wait();
                bail!("步骤超时（上限 {} 秒），已终止", timeout.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    Ok(child.wait_with_output()?)
}

// ── install ──

fn install(storage_root: &Path, project_id: &str) -> Result<InstallResult> {
    // 与 build 共用互斥：安装期间（含确认等待）同项目不得并发构建/安装，
    // 防读到半成品 release 或暂存/导入竞争。
    {
        let mut locks = BUILD_LOCKS.lock().expect("plugin-dev 构建锁中毒");
        if !locks.insert(project_id.to_string()) {
            bail!("项目 {project_id} 正在构建或安装中，请等待完成后再试");
        }
    }
    let result = install_locked(storage_root, project_id);
    let _ = BUILD_LOCKS
        .lock()
        .expect("plugin-dev 构建锁中毒")
        .remove(project_id);
    result
}

fn install_locked(storage_root: &Path, project_id: &str) -> Result<InstallResult> {
    let project_dir = dev_project_dir(storage_root, project_id)?;
    let release_dir = project_dir.join("release");
    let release_manifest = release_dir.join("plugin.json");
    if !release_manifest.is_file() {
        bail!(
            "项目 {project_id} 尚无构建产物（{} 不存在），先执行构建",
            release_manifest.display()
        );
    }
    let manifest = PluginManifest::load(&release_manifest)
        .with_context(|| format!("构建产物清单无效: {}", release_manifest.display()))?;
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
    let staged = crate::artifacts::stage_local_plugin(storage_root, &release_dir)?;
    let status = crate::registry::import_staged_plugin(storage_root, staged.path())?;
    tracing::info!(plugin = %status.id, version = %status.manifest_version, "plugin-dev 安装完成");
    Ok(InstallResult {
        plugin_id: status.id,
        version: status.manifest_version,
        state: status.state,
        enabled: status.enabled,
    })
}

// ── logs ──

fn logs(storage_root: &Path, request: &LogsRequest) -> Result<LogsResult> {
    let lines = request.lines.clamp(1, 1000);
    let path = match request.target.split_once(':') {
        Some(("dev", id)) => {
            validate_project_id(id)?;
            dev_project_dir(storage_root, id)?
                .join("logs")
                .join(BUILD_LOG_FILE)
        }
        Some(("plugin", id)) => {
            validate_project_id(id)?;
            // 只读已安装插件的运行日志目录。
            let log_dir = storage_root.join("plugins").join(id).join("logs");
            let Some(file) = newest_log_file(&log_dir) else {
                bail!(
                    "插件 {id} 暂无运行日志（{} 不存在或为空）",
                    log_dir.display()
                );
            };
            file
        }
        _ => bail!(
            "日志目标格式必须是 dev:<项目id> 或 plugin:<插件id>，收到 {}",
            request.target
        ),
    };
    if !path.is_file() {
        bail!("日志文件不存在: {}", path.display());
    }
    let content = read_log_tail(&path, LOG_TAIL_MAX_BYTES);
    let tail: Vec<String> = content
        .lines()
        .rev()
        .take(lines)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(LogsResult {
        path: path.display().to_string(),
        lines: tail,
    })
}

fn newest_log_file(log_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(log_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_log = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".log"));
        if !is_log {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(time, _)| modified > *time) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// 读取文件尾部（最多 `max_bytes` 字节，起点对齐行首），返回 UTF-8 容错文本。
fn read_log_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return String::new();
    };
    let start = size.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buffer);
    // 起点（或解码残缺）可能落在行中间，丢弃不完整首行。
    let text = if start > 0 {
        match text.find('\n') {
            Some(idx) => text[idx + 1..].to_string(),
            None => String::new(),
        }
    } else {
        text.to_string()
    };
    // 尾部截断：窗口超出时按保守字符数上限截尾，起点对齐字符边界
    // （中文等多字节字符落在切片起点会 panic）。
    let max_chars = (max_bytes / 4).max(1024) as usize;
    if text.len() > max_chars {
        let cutoff = text.len() - max_chars;
        let start = text
            .char_indices()
            .map(|(index, _)| index)
            .find(|&index| index >= cutoff)
            .unwrap_or(text.len());
        text[start..].to_string()
    } else {
        text
    }
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

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("临时目录");
        let caller = root.path().join("plugins").join("creator-demo");
        std::fs::create_dir_all(caller.join("templates/ui-app")).expect("模板目录");
        std::fs::write(
            caller.join("templates/ui-app/plugin.json"),
            r#"{"schema_version":2,"id":"{{PLUGIN_ID}}","version":"0.1.0","permissions":["bridge.call"],"ui":{"contributions":[{"slot":"extension.tab","id":"app","title":"{{PLUGIN_NAME}}","entry":"app/index.html"}]}}"#,
        )
        .expect("模板清单");
        std::fs::create_dir_all(caller.join("templates/ui-app/app")).expect("app 目录");
        std::fs::write(
            caller.join("templates/ui-app/app/index.html"),
            "<html data-plugin=\"{{PLUGIN_ID}}\">{{PLUGIN_NAME}}</html>",
        )
        .expect("模板入口");
        (root, caller)
    }

    #[test]
    fn 项目id白名单拒绝路径逃逸() {
        assert!(validate_project_id("my-plugin_1.0").is_ok());
        for bad in ["", "../escape", "a/b", "a\\b", "空 格", ".."] {
            assert!(validate_project_id(bad).is_err(), "应当拒绝 {bad:?}");
        }
    }

    #[test]
    fn init_替换占位符并写元数据() {
        let (root, caller) = setup();
        let request = InitRequest {
            template: "ui-app".into(),
            id: "demo-app".into(),
            name: Some("演示应用".into()),
        };
        let result = init(root.path(), &caller, &request).expect("init 应成功");
        assert_eq!(result.files, 2);
        let manifest =
            std::fs::read_to_string(result.directory.clone() + "/plugin.json").expect("项目清单");
        assert!(manifest.contains("\"id\":\"demo-app\""));
        assert!(manifest.contains("演示应用"));
        let html = std::fs::read_to_string(result.directory.clone() + "/app/index.html").unwrap();
        assert_eq!(html, "<html data-plugin=\"demo-app\">演示应用</html>");
        assert!(
            Path::new(&result.directory)
                .join(PROJECT_META_FILE)
                .is_file()
        );
    }

    #[test]
    fn init_拒绝同名已安装插件与自举() {
        let (root, caller) = setup();
        // 已安装同名插件
        std::fs::create_dir_all(root.path().join("plugins/occupied")).unwrap();
        std::fs::write(
            root.path().join("plugins/occupied/plugin.json"),
            r#"{"schema_version":2,"id":"occupied","version":"1.0.0","ui":{"contributions":[{"slot":"extension.tab","id":"x","entry":"x.html"}]}}"#,
        )
        .unwrap();
        let err = init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "occupied".into(),
                name: None,
            },
        )
        .expect_err("应拒绝已占用 ID");
        assert!(err.to_string().contains("已被已安装插件占用"));
        // 自举：项目 ID 与调用方相同
        let err = init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "creator-demo".into(),
                name: None,
            },
        )
        .expect_err("应拒绝自举");
        assert!(err.to_string().contains("防自举"));
    }

    #[test]
    fn init_拒绝未知模板与重复初始化() {
        let (root, caller) = setup();
        let err = init(
            root.path(),
            &caller,
            &InitRequest {
                template: "nope".into(),
                id: "demo".into(),
                name: None,
            },
        )
        .expect_err("应拒绝未知模板");
        assert!(err.to_string().contains("模板 nope 不存在"));
        init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "demo".into(),
                name: None,
            },
        )
        .expect("首次 init");
        let err = init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "demo".into(),
                name: None,
            },
        )
        .expect_err("应拒绝重复 init");
        assert!(err.to_string().contains("已存在"));
    }

    #[test]
    fn list_返回项目与安装状态() {
        let (root, caller) = setup();
        init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "demo".into(),
                name: Some("演示".into()),
            },
        )
        .unwrap();
        let entries = list(root.path()).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "demo");
        assert_eq!(entries[0].name, "演示");
        assert_eq!(entries[0].source_version.as_deref(), Some("0.1.0"));
        assert!(entries[0].installed_version.is_none());
    }

    #[test]
    fn validate_校验清单与入口提示() {
        let (root, caller) = setup();
        init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "demo".into(),
                name: None,
            },
        )
        .unwrap();
        let result = validate(root.path(), "demo").expect("validate");
        assert!(result.ok, "errors: {:?}", result.errors);
        // ui-app 模板入口 app/index.html 存在，不应有入口 warning
        assert!(
            result.warnings.is_empty(),
            "warnings: {:?}",
            result.warnings
        );
        // 破坏清单后应报错
        let dir = dev_project_dir(root.path(), "demo").unwrap();
        std::fs::write(dir.join("plugin.json"), "{ not json").unwrap();
        let result = validate(root.path(), "demo").expect("validate");
        assert!(!result.ok);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn logs_读取尾部并对齐行首() {
        let (root, _caller) = setup();
        let dir = dev_project_dir(root.path(), "demo").unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let mut content = String::new();
        for index in 0..100 {
            content.push_str(&format!("line-{index:03}\n"));
        }
        std::fs::write(dir.join("logs").join(BUILD_LOG_FILE), &content).unwrap();
        let result = logs(
            root.path(),
            &LogsRequest {
                target: "dev:demo".into(),
                lines: 3,
            },
        )
        .expect("logs");
        assert_eq!(result.lines, vec!["line-097", "line-098", "line-099"]);
        // 目标格式非法
        assert!(
            logs(
                root.path(),
                &LogsRequest {
                    target: "demo".into(),
                    lines: 3
                }
            )
            .is_err()
        );
        assert!(
            logs(
                root.path(),
                &LogsRequest {
                    target: "dev:../x".into(),
                    lines: 3
                }
            )
            .is_err()
        );
    }

    #[test]
    fn status_未初始化返回不存在() {
        let (root, _caller) = setup();
        let result = status(root.path(), "ghost").expect("status");
        assert!(!result.exists);
    }

    #[test]
    fn 零构建打包_生成产物与内容清单() {
        let (root, caller) = setup();
        init(
            root.path(),
            &caller,
            &InitRequest {
                template: "ui-app".into(),
                id: "demo".into(),
                name: Some("演示".into()),
            },
        )
        .unwrap();
        let project_dir = dev_project_dir(root.path(), "demo").unwrap();
        // ui-app 模板无 package.json，走零构建路径。
        assert!(!project_dir.join("package.json").exists());
        run_build_steps(&project_dir).expect("零构建打包");
        let release_dir = project_dir.join("release");
        assert!(release_dir.join("plugin.json").is_file());
        assert!(release_dir.join("app/index.html").is_file());
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(release_dir.join("content-manifest.json")).unwrap(),
        )
        .unwrap();
        let files = manifest["files"].as_array().expect("清单 files");
        assert_eq!(files.len(), 2); // 测试模板的 plugin.json + app/index.html
        for file in files {
            assert!(file["path"].as_str().is_some_and(|path| !path.is_empty()));
            assert_eq!(file["sha256"].as_str().map(str::len), Some(64));
        }
        // 清单中不含清单自身
        assert!(
            files
                .iter()
                .all(|file| file["path"] != "content-manifest.json")
        );
        // 构建日志已落盘
        assert!(project_dir.join("logs").join(BUILD_LOG_FILE).is_file());
    }

    /// P1 回归：多字节字符日志超出截断窗口时，尾部切片不得落在字符中间。
    #[test]
    fn 日志尾部截断对齐utf8字符边界() {
        let root = tempfile::tempdir().expect("临时目录");
        let log_path = root.path().join("build.log");
        // 每行 16 个汉字（48 字节），写 100 行 ≈ 4.8KB，
        // 触发 4KB 窗口的尾部截断（max_chars=1024，字节窗口起点在汉字中间）。
        let mut content = String::new();
        for index in 0..100 {
            content.push_str(&format!("{}\n", "构建失败请检查依赖配置".repeat(2)));
            content.push_str(&format!("line-{index:03}\n"));
        }
        std::fs::write(&log_path, &content).unwrap();
        // 修复前此处 panic：byte index is not a char boundary
        let tail = read_log_tail(&log_path, 4096);
        assert!(!tail.is_empty());
        assert!(tail.contains("line-099"), "尾部应包含最后一行");
        // 5MB 窗口（max_chars 远大于 1MB）同样验证
        let big_tail = read_log_tail(&log_path, 5 * 1024 * 1024);
        assert!(big_tail.contains("line-099"));
    }

    /// P2 回归：UI 入口无目录前缀（如 index.html）时零构建按单文件复制。
    #[test]
    fn 零构建打包_无目录前缀入口按单文件复制() {
        let root = tempfile::tempdir().expect("临时目录");
        let project_dir = root.path().join("plugins-dev").join("flat");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"flat","version":"0.1.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"index.html"}]}}"#,
        )
        .unwrap();
        std::fs::write(project_dir.join("index.html"), "<html></html>").unwrap();
        run_build_steps(&project_dir).expect("零构建打包");
        let release_dir = project_dir.join("release");
        assert!(release_dir.join("plugin.json").is_file());
        assert!(
            release_dir.join("index.html").is_file(),
            "无目录前缀的入口应按单文件复制进 release"
        );
    }

    /// P3 回归：install 与 build 共用互斥，同项目并发被拒。
    #[test]
    fn install_与build共用互斥() {
        let (root, _caller) = setup();
        BUILD_LOCKS
            .lock()
            .expect("plugin-dev 构建锁中毒")
            .insert("busy".to_string());
        let err = install(root.path(), "busy").expect_err("应拒绝并发 install");
        assert!(err.to_string().contains("正在构建或安装中"));
        BUILD_LOCKS
            .lock()
            .expect("plugin-dev 构建锁中毒")
            .remove("busy");
    }
}
