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
    deploy_atomically(&staging, &destination)?;
    eprintln!("[xtask] Memory 插件已部署到: {}", destination.display());
    Ok(())
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
