//! 天工辅助构建任务。

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const PROTOCOL_CRATE: &str = "tiangong-plugin-memory-protocol";
const WASM_CRATE: &str = "tiangong-plugin-memory-wasm";
const WASM_TARGET: &str = "wasm32-wasip2";
const WASM_ARTIFACT: &str = "tiangong_plugin_memory_wasm.wasm";
const SIDECAR_CRATE: &str = "tiangong-plugin-memory-sidecar";
const SIDECAR_ARTIFACT: &str = "tiangong-memory-sidecar";
const PLUGIN_ID: &str = "memory";
const PLUGIN_ROOT: &str = "crates/plugins/tiangong-plugin-memory";
const PLUGIN_MANIFEST: &str = "crates/plugins/tiangong-plugin-memory/plugin.json";
const PROTOCOL_MANIFEST: &str = "crates/plugins/tiangong-plugin-memory/protocol/Cargo.toml";
const RUNTIME_MANIFEST: &str = "crates/tiangong-plugin-runtime/Cargo.toml";
const PLUGIN_DIST: &str = "target/plugin-dist";
const DEFAULT_OSS_BASE_URL: &str = "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com";
const PRESERVED_DIRS: [&str; 3] = ["runtime", "logs", "data"];

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [command, plugin] if command == "build-plugin" => build_plugin(plugin),
        [command] if command == "build-wasm" || command == "build-sidecar" => {
            eprintln!("[xtask] {command} 已合并到 build-plugin memory");
            build_plugin(PLUGIN_ID)
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
    eprintln!("用法: cargo run -p xtask -- build-plugin memory");
}

fn build_plugin(plugin: &str) -> io::Result<()> {
    if plugin != PLUGIN_ID {
        return Err(invalid_input(format!("暂不支持插件: {plugin}")));
    }

    let workspace_root = workspace_root();
    validate_versions(&workspace_root)?;

    eprintln!("[xtask] 检查 Memory 私有协议（native）...");
    run_cargo(&workspace_root, &["check", "-p", PROTOCOL_CRATE])?;
    eprintln!("[xtask] 检查 Memory 私有协议（{WASM_TARGET}）...");
    run_cargo(
        &workspace_root,
        &["check", "-p", PROTOCOL_CRATE, "--target", WASM_TARGET],
    )?;
    eprintln!("[xtask] 构建 Memory WASM...");
    run_cargo(
        &workspace_root,
        &[
            "build",
            "-p",
            WASM_CRATE,
            "--target",
            WASM_TARGET,
            "--release",
        ],
    )?;
    eprintln!("[xtask] 构建 Memory sidecar...");
    run_cargo(
        &workspace_root,
        &["build", "-p", SIDECAR_CRATE, "--release"],
    )?;

    let wasm = workspace_root
        .join("target")
        .join(WASM_TARGET)
        .join("release")
        .join(WASM_ARTIFACT);
    let sidecar = workspace_root.join("target").join("release").join(format!(
        "{SIDECAR_ARTIFACT}{}",
        std::env::consts::EXE_SUFFIX
    ));
    require_file(&wasm)?;
    require_file(&sidecar)?;

    let plugins_dir = storage_root().join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let staging = plugins_dir.join(format!(".{PLUGIN_ID}-staging-{}", std::process::id()));
    let destination = plugins_dir.join(PLUGIN_ID);
    remove_dir_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;

    let staged_wasm = staging.join(WASM_ARTIFACT);
    let staged_sidecar = staging.join(format!(
        "{SIDECAR_ARTIFACT}{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&wasm, &staged_wasm)?;
    std::fs::copy(&sidecar, &staged_sidecar)?;
    std::fs::copy(
        workspace_root.join(PLUGIN_MANIFEST),
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
    generate_oss_distribution(&workspace_root, &staging)?;
    deploy_atomically(&staging, &destination)?;
    eprintln!("[xtask] Memory 插件已部署到: {}", destination.display());
    Ok(())
}

fn generate_oss_distribution(workspace_root: &Path, plugin: &Path) -> io::Result<()> {
    let manifest_path = plugin.join("plugin.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", manifest_path.display())))?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    let platform = current_platform_key();
    let dist_root = workspace_root.join(PLUGIN_DIST);
    let release_root = dist_root.join("plugins").join(PLUGIN_ID).join(version);
    let platform_root = release_root.join(&platform);
    let index_root = dist_root.join("plugins-index");
    std::fs::create_dir_all(&platform_root)?;
    std::fs::create_dir_all(index_root.join("fragments"))?;

    let dist_manifest = release_root.join("plugin.json");
    let dist_wasm = release_root.join(WASM_ARTIFACT);
    let dist_sidecar = platform_root.join(format!(
        "{SIDECAR_ARTIFACT}{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&manifest_path, &dist_manifest)?;
    std::fs::copy(plugin.join(WASM_ARTIFACT), &dist_wasm)?;
    std::fs::copy(
        plugin.join(format!(
            "{SIDECAR_ARTIFACT}{}",
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
    let release_url = format!("{base_url}/plugins/{PLUGIN_ID}/{version}");
    let release = serde_json::json!({
        "id": PLUGIN_ID,
        "name": "Memory",
        "version": version,
        "description": "对话记忆、召回与数据管理",
        "manifest": {
            "url": format!("{release_url}/plugin.json"),
            "checksum": manifest_checksum,
        },
        "wasm": {
            "url": format!("{release_url}/{WASM_ARTIFACT}"),
            "checksum": wasm_checksum,
        },
        "sidecars": {
            platform.clone(): {
                "url": format!(
                    "{release_url}/{platform}/{SIDECAR_ARTIFACT}{}",
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
            .join(format!("{PLUGIN_ID}-{platform}.json")),
        &release,
    )?;

    let checksums = format!(
        "{}  plugin.json\n{}  {}\n{}  {}/{}{}\n",
        sha256(&dist_manifest)?,
        sha256(&dist_wasm)?,
        WASM_ARTIFACT,
        sha256(&dist_sidecar)?,
        platform,
        SIDECAR_ARTIFACT,
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

fn validate_versions(workspace_root: &Path) -> io::Result<()> {
    let workspace = read_toml(&workspace_root.join("Cargo.toml"))?;
    let workspace_version = workspace
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid_data("无法读取 workspace.package.version"))?;

    let manifest_path = workspace_root.join(PLUGIN_MANIFEST);
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

    let protocol = read_toml(&workspace_root.join(PROTOCOL_MANIFEST))?;
    let business_protocol = protocol
        .get("package")
        .and_then(|value| value.get("metadata"))
        .and_then(|value| value.get("tiangong"))
        .and_then(|value| value.get("business-protocol"))
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| invalid_data("Memory Protocol 缺少 business-protocol 元数据"))?;
    let manifest_business_protocol = manifest
        .get("sidecar")
        .and_then(|value| value.get("business_protocol"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid_data("plugin.json 缺少 sidecar.business_protocol"))?;
    if u64::try_from(business_protocol).ok() != Some(manifest_business_protocol) {
        return Err(invalid_data(format!(
            "Memory 业务协议版本不一致: protocol={business_protocol}, plugin.json={manifest_business_protocol}"
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
    if wasm_binary != Some(WASM_ARTIFACT) || sidecar_binary != Some(SIDECAR_ARTIFACT) {
        return Err(invalid_data("plugin.json 制品名称与构建产物不一致"));
    }

    let plugin_root = workspace_root.join(PLUGIN_ROOT);
    require_file(&plugin_root.join("wasm/Cargo.toml"))?;
    require_file(&plugin_root.join("sidecar/Cargo.toml"))?;
    require_file(&plugin_root.join("protocol/Cargo.toml"))?;
    Ok(())
}

fn deploy_atomically(staging: &Path, destination: &Path) -> io::Result<()> {
    if !destination.exists() {
        return std::fs::rename(staging, destination);
    }

    let parent = destination
        .parent()
        .ok_or_else(|| invalid_input("插件安装目录缺少父目录"))?;
    let backup = parent.join(format!(".{PLUGIN_ID}-backup-{}", std::process::id()));
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
