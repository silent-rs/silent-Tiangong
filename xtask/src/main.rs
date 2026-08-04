//! 天工辅助构建任务。

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const WASM_TARGET: &str = "wasm32-wasip2";
const RUNTIME_MANIFEST: &str = "crates/tiangong-plugin-runtime/Cargo.toml";
const PLUGIN_DIST: &str = "target/plugin-dist";
const DEFAULT_OSS_BASE_URL: &str = "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com";
const PRESERVED_DIRS: [&str; 3] = ["runtime", "logs", "data"];

/// 单个 WASM 插件的构建配置。
struct PluginConfig {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    protocol_crate: &'static str,
    wasm_crate: &'static str,
    wasm_artifact: &'static str,
    sidecar_crate: &'static str,
    sidecar_artifact: &'static str,
    plugin_root: &'static str,
    plugin_manifest: &'static str,
    protocol_manifest: &'static str,
}

const MEMORY: PluginConfig = PluginConfig {
    id: "memory",
    name: "Memory",
    description: "对话记忆、召回与数据管理",
    protocol_crate: "tiangong-plugin-memory-protocol",
    wasm_crate: "tiangong-plugin-memory-wasm",
    wasm_artifact: "tiangong_plugin_memory_wasm.wasm",
    sidecar_crate: "tiangong-plugin-memory-sidecar",
    sidecar_artifact: "tiangong-memory-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-memory",
    plugin_manifest: "crates/plugins/tiangong-plugin-memory/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-memory/protocol/Cargo.toml",
};

const MCP: PluginConfig = PluginConfig {
    id: "mcp",
    name: "MCP",
    description: "MCP server 管理与工具桥接",
    protocol_crate: "tiangong-plugin-mcp-protocol",
    wasm_crate: "tiangong-plugin-mcp-wasm",
    wasm_artifact: "tiangong_plugin_mcp_wasm.wasm",
    sidecar_crate: "tiangong-plugin-mcp-sidecar",
    sidecar_artifact: "tiangong-mcp-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-mcp",
    plugin_manifest: "crates/plugins/tiangong-plugin-mcp/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-mcp/protocol/Cargo.toml",
};

const INDEX: PluginConfig = PluginConfig {
    id: "index",
    name: "Index",
    description: "工作区文件索引、对话历史索引与代码检索",
    protocol_crate: "tiangong-plugin-index-protocol",
    wasm_crate: "tiangong-plugin-index-wasm",
    wasm_artifact: "tiangong_plugin_index_wasm.wasm",
    sidecar_crate: "tiangong-plugin-index-sidecar",
    sidecar_artifact: "tiangong-index-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-index",
    plugin_manifest: "crates/plugins/tiangong-plugin-index/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-index/protocol/Cargo.toml",
};

const SCHEDULER: PluginConfig = PluginConfig {
    id: "scheduler",
    name: "Scheduler",
    description: "定时任务调度与执行",
    protocol_crate: "tiangong-plugin-scheduler-protocol",
    wasm_crate: "tiangong-plugin-scheduler-wasm",
    wasm_artifact: "tiangong_plugin_scheduler_wasm.wasm",
    sidecar_crate: "tiangong-plugin-scheduler-sidecar",
    sidecar_artifact: "tiangong-scheduler-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-scheduler",
    plugin_manifest: "crates/plugins/tiangong-plugin-scheduler/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-scheduler/protocol/Cargo.toml",
};

const SKILL: PluginConfig = PluginConfig {
    id: "skill",
    name: "Skill",
    description: "Skill 注册表管理、详情查询与 prompt 段落注入",
    protocol_crate: "tiangong-plugin-skill-protocol",
    wasm_crate: "tiangong-plugin-skill-wasm",
    wasm_artifact: "tiangong_plugin_skill_wasm.wasm",
    sidecar_crate: "tiangong-plugin-skill-sidecar",
    sidecar_artifact: "tiangong-skill-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-skill",
    plugin_manifest: "crates/plugins/tiangong-plugin-skill/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-skill/protocol/Cargo.toml",
};

const CODING: PluginConfig = PluginConfig {
    id: "coding",
    name: "Coding",
    description: "通用开发工作流、项目上下文与交付审查",
    protocol_crate: "tiangong-plugin-coding-protocol",
    wasm_crate: "tiangong-plugin-coding-wasm",
    wasm_artifact: "tiangong_plugin_coding_wasm.wasm",
    sidecar_crate: "tiangong-plugin-coding-sidecar",
    sidecar_artifact: "tiangong-coding-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-coding",
    plugin_manifest: "crates/plugins/tiangong-plugin-coding/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-coding/protocol/Cargo.toml",
};

const TEXT_TO_SPEECH: PluginConfig = PluginConfig {
    id: "text-to-speech",
    name: "Text-To-Speech",
    description: "文本转语音",
    protocol_crate: "tiangong-plugin-text-to-speech-protocol",
    wasm_crate: "tiangong-plugin-text-to-speech-wasm",
    wasm_artifact: "tiangong_plugin_text_to_speech_wasm.wasm",
    sidecar_crate: "tiangong-plugin-text-to-speech-sidecar",
    sidecar_artifact: "tiangong-text-to-speech-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-text-to-speech",
    plugin_manifest: "crates/plugins/tiangong-plugin-text-to-speech/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-text-to-speech/protocol/Cargo.toml",
};

const GENERATE_IMAGE: PluginConfig = PluginConfig {
    id: "generate-image",
    name: "Generate-Image",
    description: "图片生成",
    protocol_crate: "tiangong-plugin-generate-image-protocol",
    wasm_crate: "tiangong-plugin-generate-image-wasm",
    wasm_artifact: "tiangong_plugin_generate_image_wasm.wasm",
    sidecar_crate: "tiangong-plugin-generate-image-sidecar",
    sidecar_artifact: "tiangong-generate-image-sidecar",
    plugin_root: "crates/plugins/tiangong-plugin-generate-image",
    plugin_manifest: "crates/plugins/tiangong-plugin-generate-image/plugin.json",
    protocol_manifest: "crates/plugins/tiangong-plugin-generate-image/protocol/Cargo.toml",
};

fn plugin_config(id: &str) -> io::Result<&'static PluginConfig> {
    match id {
        "memory" => Ok(&MEMORY),
        "mcp" => Ok(&MCP),
        "index" => Ok(&INDEX),
        "scheduler" => Ok(&SCHEDULER),
        "skill" => Ok(&SKILL),
        "coding" => Ok(&CODING),
        "text-to-speech" => Ok(&TEXT_TO_SPEECH),
        "generate-image" => Ok(&GENERATE_IMAGE),
        other => Err(invalid_input(format!("暂不支持插件: {other}"))),
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [command, plugin] if command == "build-plugin" => match plugin_config(plugin) {
            Ok(config) => build_plugin(config),
            Err(error) => Err(error),
        },
        [command] if command == "build-wasm" || command == "build-sidecar" => {
            eprintln!("[xtask] {command} 已合并到 build-plugin <id>");
            Err(invalid_input("请使用 build-plugin <id>"))
        }
        [command] if command == "help" || command == "--help" || command == "-h" => {
            print_help();
            Ok(())
        }
        [] => {
            print_help();
            Ok(())
        }
        _ => {
            print_help();
            Err(invalid_input("参数无效"))
        }
    };

    if let Err(error) = result {
        eprintln!("xtask 失败: {error}");
        std::process::exit(1);
    }
}

fn print_help() {
    eprintln!("xtask - 天工辅助构建任务\n");
    eprintln!("用法: cargo run -p xtask -- build-plugin <id>");
    eprintln!("支持的插件: memory, mcp, index, scheduler, skill, coding");
}

fn build_plugin(config: &PluginConfig) -> io::Result<()> {
    let workspace_root = workspace_root();
    validate_versions(&workspace_root, config)?;

    let plugin_name = config.name;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（native）...");
    run_cargo(&workspace_root, &["check", "-p", config.protocol_crate])?;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（{WASM_TARGET}）...");
    run_cargo(
        &workspace_root,
        &[
            "check",
            "-p",
            config.protocol_crate,
            "--target",
            WASM_TARGET,
        ],
    )?;
    eprintln!("[xtask] 构建 {plugin_name} WASM...");
    run_cargo(
        &workspace_root,
        &[
            "build",
            "-p",
            config.wasm_crate,
            "--target",
            WASM_TARGET,
            "--release",
        ],
    )?;
    eprintln!("[xtask] 构建 {plugin_name} sidecar...");
    run_cargo(
        &workspace_root,
        &["build", "-p", config.sidecar_crate, "--release"],
    )?;

    let wasm = workspace_root
        .join("target")
        .join(WASM_TARGET)
        .join("release")
        .join(config.wasm_artifact);
    let sidecar = workspace_root.join("target").join("release").join(format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    ));
    require_file(&wasm)?;
    require_file(&sidecar)?;

    let plugins_dir = storage_root().join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let staging = plugins_dir.join(format!(".{}-staging-{}", config.id, std::process::id()));
    let destination = plugins_dir.join(config.id);
    remove_dir_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;

    let staged_wasm = staging.join(config.wasm_artifact);
    let staged_sidecar = staging.join(format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&wasm, &staged_wasm)?;
    std::fs::copy(&sidecar, &staged_sidecar)?;
    std::fs::copy(
        workspace_root.join(config.plugin_manifest),
        staging.join("plugin.json"),
    )?;
    for directory in PRESERVED_DIRS {
        std::fs::create_dir_all(staging.join(directory))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged_sidecar, std::fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("[xtask] WASM sha256: {}", sha256(&staged_wasm)?);
    eprintln!("[xtask] sidecar sha256: {}", sha256(&staged_sidecar)?);
    generate_oss_distribution(&workspace_root, &staging, config)?;
    deploy_atomically(&staging, &destination, config)?;
    eprintln!(
        "[xtask] {plugin_name} 插件已部署到: {}",
        destination.display()
    );
    Ok(())
}

fn generate_oss_distribution(
    workspace_root: &Path,
    plugin: &Path,
    config: &PluginConfig,
) -> io::Result<()> {
    let manifest_path = plugin.join("plugin.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", manifest_path.display())))?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    let platform = current_platform_key();
    let dist_root = workspace_root.join(PLUGIN_DIST);
    let release_root = dist_root.join("plugins").join(config.id).join(version);
    let platform_root = release_root.join(&platform);
    let index_root = dist_root.join("plugins-index");
    std::fs::create_dir_all(&platform_root)?;
    std::fs::create_dir_all(index_root.join("fragments"))?;

    let dist_manifest = release_root.join("plugin.json");
    let dist_wasm = release_root.join(config.wasm_artifact);
    let dist_sidecar = platform_root.join(format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&manifest_path, &dist_manifest)?;
    std::fs::copy(plugin.join(config.wasm_artifact), &dist_wasm)?;
    std::fs::copy(
        plugin.join(format!(
            "{}{}",
            config.sidecar_artifact,
            std::env::consts::EXE_SUFFIX
        )),
        &dist_sidecar,
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dist_sidecar, std::fs::Permissions::from_mode(0o755))?;
    }

    let manifest_checksum = format!("sha256:{}", sha256(&dist_manifest)?);
    let wasm_checksum = format!("sha256:{}", sha256(&dist_wasm)?);
    let sidecar_checksum = format!("sha256:{}", sha256(&dist_sidecar)?);
    let base_url = env_var_or("TIANGONG_PLUGIN_OSS_BASE_URL", DEFAULT_OSS_BASE_URL)
        .trim_end_matches('/')
        .to_string();
    let release_url = format!("{base_url}/plugins/{}/{}", config.id, version);
    let release = serde_json::json!({
        "id": config.id,
        "name": config.name,
        "version": version,
        "description": config.description,
        "manifest": {
            "url": format!("{release_url}/plugin.json"),
            "checksum": manifest_checksum,
        },
        "wasm": {
            "url": format!("{release_url}/{}", config.wasm_artifact),
            "checksum": wasm_checksum,
        },
        "sidecars": {
            platform.clone(): {
                "url": format!(
                    "{release_url}/{platform}/{}{}",
                    config.sidecar_artifact,
                    std::env::consts::EXE_SUFFIX
                ),
                "checksum": sidecar_checksum,
            }
        }
    });
    write_json(
        &index_root.join("catalog.json"),
        &serde_json::json!({"version": 1, "plugins": [release.clone()]}),
    )?;
    write_json(
        &index_root
            .join("fragments")
            .join(format!("{}-{platform}.json", config.id)),
        &release,
    )?;

    let checksums = format!(
        "{}  plugin.json\n{}  {}\n{}  {}/{}{}\n",
        sha256(&dist_manifest)?,
        sha256(&dist_wasm)?,
        config.wasm_artifact,
        sha256(&dist_sidecar)?,
        platform,
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX,
    );
    std::fs::write(
        release_root.join(format!("SHA256SUMS-{platform}")),
        checksums,
    )?;
    eprintln!("[xtask] OSS 制品已生成: {}", dist_root.display());
    Ok(())
}

fn write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut content = serde_json::to_vec_pretty(value)
        .map_err(|error| invalid_data(format!("生成 {} 失败: {error}", path.display())))?;
    content.push(b'\n');
    std::fs::write(path, content)
}

fn current_platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}")
}

fn validate_versions(workspace_root: &Path, config: &PluginConfig) -> io::Result<()> {
    let workspace = read_toml(&workspace_root.join("Cargo.toml"))?;
    let workspace_version = workspace
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid_data("无法读取 workspace.package.version"))?;

    let manifest_path = workspace_root.join(config.plugin_manifest);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", manifest_path.display())))?;
    let manifest_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    if manifest_version != workspace_version {
        return Err(invalid_data(format!(
            "插件版本不一致: workspace={workspace_version}, plugin.json={manifest_version}"
        )));
    }

    let protocol = read_toml(&workspace_root.join(config.protocol_manifest))?;
    let business_protocol = protocol
        .get("package")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("tiangong"))
        .and_then(|value| value.get("business-protocol"))
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| invalid_data("Protocol 缺少 business-protocol 元数据"))?;
    let manifest_business_protocol = manifest
        .get("sidecar")
        .and_then(|value| value.get("business_protocol"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid_data("plugin.json 缺少 sidecar.business_protocol"))?;
    if u64::try_from(business_protocol).ok() != Some(manifest_business_protocol) {
        return Err(invalid_data(format!(
            "业务协议版本不一致: protocol={business_protocol}, plugin.json={manifest_business_protocol}"
        )));
    }

    let runtime = read_toml(&workspace_root.join(RUNTIME_MANIFEST))?;
    let transport_protocol = runtime
        .get("package")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("tiangong"))
        .and_then(|value| value.get("transport-protocol"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid_data("插件运行时缺少 transport-protocol 元数据"))?;
    let manifest_transport_protocol = manifest
        .get("sidecar")
        .and_then(|value| value.get("transport_protocol"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 sidecar.transport_protocol"))?;
    if transport_protocol != manifest_transport_protocol {
        return Err(invalid_data(format!(
            "sidecar transport 版本不一致: runtime={transport_protocol}, plugin.json={manifest_transport_protocol}"
        )));
    }

    let wasm_binary = manifest
        .get("wasm")
        .and_then(|value| value.get("binary"))
        .and_then(serde_json::Value::as_str);
    let sidecar_binary = manifest
        .get("sidecar")
        .and_then(|value| value.get("binary"))
        .and_then(serde_json::Value::as_str);
    if wasm_binary != Some(config.wasm_artifact) || sidecar_binary != Some(config.sidecar_artifact)
    {
        return Err(invalid_data("plugin.json 制品名称与构建产物不一致"));
    }

    let plugin_root = workspace_root.join(config.plugin_root);
    require_file(&plugin_root.join("wasm/Cargo.toml"))?;
    require_file(&plugin_root.join("sidecar/Cargo.toml"))?;
    require_file(&plugin_root.join("protocol/Cargo.toml"))?;
    Ok(())
}

fn deploy_atomically(staging: &Path, destination: &Path, config: &PluginConfig) -> io::Result<()> {
    if !destination.exists() {
        return std::fs::rename(staging, destination);
    }

    let parent = destination
        .parent()
        .ok_or_else(|| invalid_input("插件安装目录缺少父目录"))?;
    let backup = parent.join(format!(".{}-backup-{}", config.id, std::process::id()));
    remove_dir_if_exists(&backup)?;
    std::fs::rename(destination, &backup)?;

    let result = (|| {
        for directory in PRESERVED_DIRS {
            let staged = staging.join(directory);
            let preserved = backup.join(directory);
            if preserved.exists() {
                remove_dir_if_exists(&staged)?;
                std::fs::rename(preserved, staged)?;
            }
        }
        std::fs::rename(staging, destination)
    })();

    if let Err(error) = result {
        for directory in PRESERVED_DIRS {
            let staged = staging.join(directory);
            let preserved = backup.join(directory);
            if staged.exists() && !preserved.exists() {
                let _ = std::fs::rename(staged, preserved);
            }
        }
        let _ = std::fs::rename(&backup, destination);
        return Err(error);
    }

    remove_dir_if_exists(&backup)
}

fn run_cargo(workspace_root: &Path, args: &[&str]) -> io::Result<()> {
    let status = Command::new(env_var_or("CARGO", "cargo"))
        .current_dir(workspace_root)
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cargo {} 执行失败",
            args.join(" ")
        )))
    }
}

fn read_toml(path: &Path) -> io::Result<toml::Value> {
    let content = std::fs::read_to_string(path)?;
    toml::from_str(&content)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", path.display())))
}

fn require_file(path: &Path) -> io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("文件不存在: {}", path.display()),
        ))
    }
}

fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn sha256(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|value| !value.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buffer = PathBuf::from(drive);
            buffer.push(path);
            Some(buffer)
        }
        _ => None,
    }
}

fn env_var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
