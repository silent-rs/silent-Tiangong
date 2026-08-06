//! 天工辅助构建任务。

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const WASM_TARGET: &str = "wasm32-wasip2";
const RUNTIME_MANIFEST: &str = "crates/tiangong-plugin-runtime/Cargo.toml";
const PLUGIN_DIST: &str = "target/plugin-dist";
const DEFAULT_OSS_BASE_URL: &str = "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com";
const PRESERVED_DIRS: [&str; 3] = ["runtime", "logs", "data"];

/// 从标准插件目录发现的单个 WASM + sidecar 插件构建配置。
struct PluginConfig {
    id: String,
    name: String,
    description: String,
    protocol_crate: String,
    wasm_crate: String,
    wasm_artifact: String,
    sidecar_crate: String,
    sidecar_artifact: String,
    plugin_root: PathBuf,
    plugin_manifest: PathBuf,
    protocol_manifest: PathBuf,
}

fn plugin_config(id: &str) -> io::Result<PluginConfig> {
    validate_plugin_id(id)?;
    let workspace_root = workspace_root();
    let plugin_root = PathBuf::from("crates/plugins").join(format!("tiangong-plugin-{id}"));
    let absolute_root = workspace_root.join(&plugin_root);
    let plugin_manifest = plugin_root.join("plugin.json");
    let protocol_manifest = plugin_root.join("protocol/Cargo.toml");
    let wasm_manifest = plugin_root.join("wasm/Cargo.toml");
    let sidecar_manifest = plugin_root.join("sidecar/Cargo.toml");
    for path in [
        &plugin_manifest,
        &protocol_manifest,
        &wasm_manifest,
        &sidecar_manifest,
    ] {
        require_file(&workspace_root.join(path)).map_err(|_| {
            invalid_input(format!(
                "不是完整的 WASM + sidecar 插件目录: {}",
                absolute_root.display()
            ))
        })?;
    }

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(workspace_root.join(&plugin_manifest))?)
            .map_err(|error| invalid_data(format!("解析插件 {id} 的 plugin.json 失败: {error}")))?;
    if manifest.get("id").and_then(serde_json::Value::as_str) != Some(id) {
        return Err(invalid_data(format!(
            "目录插件 ID 与 plugin.json 不一致: expected={id}"
        )));
    }
    let protocol = read_toml(&workspace_root.join(&protocol_manifest))?;
    let wasm = read_toml(&workspace_root.join(&wasm_manifest))?;
    let sidecar = read_toml(&workspace_root.join(&sidecar_manifest))?;
    let protocol_crate = toml_package_name(&protocol, id, "protocol")?;
    let wasm_crate = toml_package_name(&wasm, id, "wasm")?;
    let sidecar_crate = toml_package_name(&sidecar, id, "sidecar")?;
    let wasm_artifact = manifest
        .pointer("/wasm/binary")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data(format!("插件 {id} 缺少 wasm.binary")))?
        .to_string();
    let sidecar_artifact = manifest
        .pointer("/sidecar/binary")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data(format!("插件 {id} 缺少 sidecar.binary")))?
        .to_string();
    let expected_wasm = format!("{}.wasm", wasm_crate.replace('-', "_"));
    if wasm_artifact != expected_wasm {
        return Err(invalid_data(format!(
            "插件 {id} 的 wasm.binary 与 WASM crate 不一致: expected={expected_wasm}, actual={wasm_artifact}"
        )));
    }
    let sidecar_bins = sidecar
        .get("bin")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| invalid_data(format!("插件 {id} sidecar Cargo.toml 缺少 [[bin]]")))?;
    if !sidecar_bins
        .iter()
        .any(|bin| bin.get("name").and_then(toml::Value::as_str) == Some(sidecar_artifact.as_str()))
    {
        return Err(invalid_data(format!(
            "插件 {id} 的 sidecar.binary 与 sidecar [[bin]] 不一致"
        )));
    }
    let description = wasm
        .get("package")
        .and_then(|value| value.get("description"))
        .and_then(toml::Value::as_str)
        .unwrap_or(id)
        .to_string();

    Ok(PluginConfig {
        id: id.to_string(),
        name: id.to_string(),
        description,
        protocol_crate,
        wasm_crate,
        wasm_artifact,
        sidecar_crate,
        sidecar_artifact,
        plugin_root,
        plugin_manifest,
        protocol_manifest,
    })
}

fn validate_plugin_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid_input(format!("插件 ID 无效: {id}")));
    }
    Ok(())
}

fn toml_package_name(value: &toml::Value, id: &str, component: &str) -> io::Result<String> {
    value
        .get("package")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            invalid_data(format!(
                "插件 {id} 的 {component} Cargo.toml 缺少 package.name"
            ))
        })
}

fn discover_plugins() -> io::Result<Vec<PluginConfig>> {
    let root = workspace_root().join("crates/plugins");
    let mut plugins = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(directory) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(id) = directory.strip_prefix("tiangong-plugin-") else {
            continue;
        };
        if let Ok(config) = plugin_config(id) {
            plugins.push(config);
        }
    }
    plugins.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(plugins)
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [command] if command == "list-plugins" => list_plugins(),
        [command, plugin] if command == "validate-plugin" => {
            plugin_config(plugin).and_then(|config| validate_versions(&workspace_root(), &config))
        }
        [command, plugin] if command == "build-plugin" => match plugin_config(plugin) {
            Ok(config) => build_plugin(&config),
            Err(error) => Err(error),
        },
        [command, input, output] if command == "merge-plugin-dist" => {
            merge_plugin_distributions(Path::new(input), Path::new(output), None)
        }
        [command, plugin, input, output] if command == "merge-plugin-dist" => plugin_config(plugin)
            .and_then(|_| {
                merge_plugin_distributions(Path::new(input), Path::new(output), Some(plugin))
            }),
        [command, current, plugin_release, output] if command == "merge-plugin-catalog" => {
            merge_plugin_catalog(
                Path::new(current),
                Path::new(plugin_release),
                Path::new(output),
            )
        }
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
    eprintln!("用法:");
    eprintln!("  cargo run -p xtask -- list-plugins");
    eprintln!("  cargo run -p xtask -- validate-plugin <id>");
    eprintln!("  cargo run -p xtask -- build-plugin <id>");
    eprintln!("  cargo run -p xtask -- merge-plugin-dist [plugin-id] <输入目录> <输出目录>");
    eprintln!(
        "  cargo run -p xtask -- merge-plugin-catalog <当前catalog或-> <插件release> <输出catalog>"
    );
    eprintln!(
        "可发布插件由 crates/plugins 下的完整 WASM + protocol + sidecar 插件配置决定；使用 list-plugins 查询。"
    );
}

fn list_plugins() -> io::Result<()> {
    for config in discover_plugins()? {
        validate_versions(&workspace_root(), &config)?;
        println!("{}", config.id);
    }
    Ok(())
}

fn build_plugin(config: &PluginConfig) -> io::Result<()> {
    let workspace_root = workspace_root();
    validate_versions(&workspace_root, config)?;

    let plugin_name = &config.name;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（native）...");
    run_cargo(&workspace_root, &["check", "-p", &config.protocol_crate])?;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（{WASM_TARGET}）...");
    run_cargo(
        &workspace_root,
        &[
            "check",
            "-p",
            &config.protocol_crate,
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
            &config.wasm_crate,
            "--target",
            WASM_TARGET,
            "--release",
        ],
    )?;
    eprintln!("[xtask] 构建 {plugin_name} sidecar...");
    run_cargo(
        &workspace_root,
        &["build", "-p", &config.sidecar_crate, "--release"],
    )?;

    let wasm = workspace_root
        .join("target")
        .join(WASM_TARGET)
        .join("release")
        .join(&config.wasm_artifact);
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
    let destination = plugins_dir.join(&config.id);
    remove_dir_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;

    let staged_wasm = staging.join(&config.wasm_artifact);
    let staged_sidecar = staging.join(format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&wasm, &staged_wasm)?;
    std::fs::copy(&sidecar, &staged_sidecar)?;
    std::fs::copy(
        workspace_root.join(&config.plugin_manifest),
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
    write_signed_release(&staging, config)?;
    generate_oss_distribution(&workspace_root, &staging, config)?;
    deploy_atomically(&staging, &destination, config)?;
    eprintln!(
        "[xtask] {plugin_name} 插件已部署到: {}",
        destination.display()
    );
    Ok(())
}

fn write_signed_release(plugin: &Path, config: &PluginConfig) -> io::Result<()> {
    let manifest_path = plugin.join("plugin.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 plugin.json 失败: {error}")))?;
    let version = manifest
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    let permissions = manifest
        .get("permissions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let sidecar_name = format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    );
    let release = serde_json::json!({
        "schema_version": 1,
        "id": config.id,
        "version": version,
        "publisher": "tiangong-official",
        "permissions": permissions,
        "manifest": { "path": "plugin.json", "sha256": sha256(&manifest_path)? },
        "wasm": { "path": config.wasm_artifact, "sha256": sha256(&plugin.join(&config.wasm_artifact))? },
        "sidecar": { "path": sidecar_name, "sha256": sha256(&plugin.join(&sidecar_name))? }
    });
    let release_path = plugin.join("release.json");
    write_json(&release_path, &release)?;
    let key_path = std::env::var_os("TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH")
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join(".tiangong/keys/plugin-signing.key")))
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            invalid_input("缺少插件签名私钥，请设置 TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH")
        })?;
    let password =
        std::env::var("TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PASSWORD").unwrap_or_default();
    let status = Command::new("cargo")
        .args(["tauri", "signer", "sign", "-f"])
        .arg(&key_path)
        .args(["-p", &password])
        .arg(&release_path)
        .status()?;
    if !status.success() {
        return Err(invalid_data("插件发布清单签名失败"));
    }
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
    let release_root = dist_root.join("plugins").join(&config.id).join(version);
    let platform_root = release_root.join(&platform);
    let index_root = dist_root.join("plugins-index");
    std::fs::create_dir_all(&platform_root)?;
    std::fs::create_dir_all(index_root.join("fragments"))?;

    let dist_manifest = release_root.join("plugin.json");
    let dist_wasm = release_root.join(&config.wasm_artifact);
    let dist_sidecar = platform_root.join(format!(
        "{}{}",
        config.sidecar_artifact,
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(&manifest_path, &dist_manifest)?;
    std::fs::copy(
        plugin.join("release.json"),
        platform_root.join("release.json"),
    )?;
    std::fs::copy(
        plugin.join("release.json.sig"),
        platform_root.join("release.json.sig"),
    )?;
    std::fs::copy(plugin.join(&config.wasm_artifact), &dist_wasm)?;
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
        "signed_releases": {
            platform.clone(): {
                "url": format!("{release_url}/{platform}/release.json"),
                "signature_url": format!("{release_url}/{platform}/release.json.sig"),
            }
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

    let manifest_path = workspace_root.join(&config.plugin_manifest);
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

    let protocol = read_toml(&workspace_root.join(&config.protocol_manifest))?;
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
    if wasm_binary != Some(config.wasm_artifact.as_str())
        || sidecar_binary != Some(config.sidecar_artifact.as_str())
    {
        return Err(invalid_data("plugin.json 制品名称与构建产物不一致"));
    }

    let plugin_root = workspace_root.join(&config.plugin_root);
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

fn merge_plugin_distributions(
    input_root: &Path,
    output_root: &Path,
    expected_plugin: Option<&str>,
) -> io::Result<()> {
    if !input_root.is_dir() {
        return Err(invalid_input(format!(
            "插件平台制品输入目录不存在: {}",
            input_root.display()
        )));
    }
    remove_dir_if_exists(output_root)?;
    std::fs::create_dir_all(output_root.join("plugins-index"))?;

    let mut fragments = Vec::new();
    collect_fragment_files(input_root, &mut fragments)?;
    if fragments.is_empty() {
        return Err(invalid_data("没有找到插件目录片段"));
    }
    fragments.sort();

    let mut releases = BTreeMap::<String, serde_json::Value>::new();
    for fragment_path in fragments {
        let release: serde_json::Value = serde_json::from_slice(&std::fs::read(&fragment_path)?)
            .map_err(|error| {
                invalid_data(format!("解析 {} 失败: {error}", fragment_path.display()))
            })?;
        let id = required_json_string(&release, "id", &fragment_path)?;
        if expected_plugin.is_some_and(|expected| expected != id) {
            continue;
        }
        let version = required_json_string(&release, "version", &fragment_path)?;
        let manifest = release
            .get("manifest")
            .cloned()
            .ok_or_else(|| invalid_data(format!("{} 缺少 manifest", fragment_path.display())))?;
        let wasm = release
            .get("wasm")
            .cloned()
            .ok_or_else(|| invalid_data(format!("{} 缺少 wasm", fragment_path.display())))?;
        let sidecars = release
            .get("sidecars")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| invalid_data(format!("{} 缺少 sidecars", fragment_path.display())))?;
        let signed_releases = release
            .get("signed_releases")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                invalid_data(format!("{} 缺少 signed_releases", fragment_path.display()))
            })?;
        if sidecars.len() != 1 || signed_releases.len() != 1 {
            return Err(invalid_data(format!(
                "{} 必须只包含一个平台的 sidecar 与签名清单",
                fragment_path.display()
            )));
        }
        let platform = sidecars.keys().next().expect("长度已检查");
        if !signed_releases.contains_key(platform) {
            return Err(invalid_data(format!(
                "{} 的 sidecar 与签名平台不一致",
                fragment_path.display()
            )));
        }

        let key = format!("{id}@{version}");
        if let Some(existing) = releases.get_mut(&key) {
            for field in ["id", "name", "version", "description"] {
                if existing.get(field) != release.get(field) {
                    return Err(invalid_data(format!(
                        "插件 {key} 的 {field} 在不同平台不一致"
                    )));
                }
            }
            if existing.get("manifest") != Some(&manifest) || existing.get("wasm") != Some(&wasm) {
                return Err(invalid_data(format!(
                    "插件 {key} 的 plugin.json 或 WASM 在不同平台不一致"
                )));
            }
            merge_platform_map(existing, &release, "sidecars", &key)?;
            merge_platform_map(existing, &release, "signed_releases", &key)?;
        } else {
            releases.insert(key, release);
        }
    }

    if let Some(plugin) = expected_plugin {
        if releases.is_empty() {
            return Err(invalid_data(format!("没有找到插件 {plugin} 的目录片段")));
        }
        if releases.len() != 1 {
            return Err(invalid_data(format!(
                "插件 {plugin} 的输入包含多个版本，独立发布一次只能发布一个版本"
            )));
        }
    }

    let expected_platforms = std::env::var("TIANGONG_PLUGIN_EXPECTED_PLATFORMS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    for (key, release) in &releases {
        let platforms = release["sidecars"]
            .as_object()
            .expect("合并前已验证 sidecars")
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if !expected_platforms.is_empty() && platforms != expected_platforms {
            return Err(invalid_data(format!(
                "插件 {key} 平台不完整: expected={expected_platforms:?}, actual={platforms:?}"
            )));
        }
    }

    if let Some(plugin) = expected_plugin {
        copy_plugin_distribution_files(input_root, output_root, plugin)?;
    } else {
        copy_distribution_files(input_root, output_root)?;
    }
    let plugins = releases.into_values().collect::<Vec<_>>();
    write_json(
        &output_root.join("plugins-index/catalog.json"),
        &serde_json::json!({"version": 1, "plugins": plugins}),
    )?;
    eprintln!(
        "[xtask] 已合并 {} 个插件的多平台 OSS 制品: {}",
        plugins.len(),
        output_root.display()
    );
    Ok(())
}

fn collect_fragment_files(directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_fragment_files(&path, output)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("json")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str())
                == Some("fragments")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn required_json_string<'a>(
    value: &'a serde_json::Value,
    field: &str,
    path: &Path,
) -> io::Result<&'a str> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_data(format!("{} 缺少 {field}", path.display())))
}

fn merge_platform_map(
    existing: &mut serde_json::Value,
    incoming: &serde_json::Value,
    field: &str,
    key: &str,
) -> io::Result<()> {
    let incoming = incoming[field]
        .as_object()
        .ok_or_else(|| invalid_data(format!("插件 {key} 的 {field} 无效")))?;
    let existing = existing[field]
        .as_object_mut()
        .ok_or_else(|| invalid_data(format!("插件 {key} 的 {field} 无效")))?;
    for (platform, artifact) in incoming {
        if existing
            .insert(platform.clone(), artifact.clone())
            .is_some()
        {
            return Err(invalid_data(format!("插件 {key} 的平台 {platform} 重复")));
        }
    }
    Ok(())
}

fn copy_distribution_files(input_root: &Path, output_root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(input_root)? {
        let entry = entry?;
        let source = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.file_name() == "plugins" {
            merge_directory(&source, &output_root.join("plugins"))?;
        } else {
            copy_distribution_files(&source, output_root)?;
        }
    }
    Ok(())
}

fn merge_directory(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            merge_directory(&source_path, &destination_path)?;
        } else if destination_path.exists() {
            if std::fs::read(&source_path)? != std::fs::read(&destination_path)? {
                return Err(invalid_data(format!(
                    "多平台制品内容冲突: {}",
                    destination_path.display()
                )));
            }
        } else {
            std::fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn copy_plugin_distribution_files(
    input_root: &Path,
    output_root: &Path,
    plugin: &str,
) -> io::Result<()> {
    for entry in std::fs::read_dir(input_root)? {
        let entry = entry?;
        let source = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.file_name() == "plugins" {
            let plugin_source = source.join(plugin);
            if plugin_source.is_dir() {
                merge_directory(&plugin_source, &output_root.join("plugins").join(plugin))?;
            }
        } else {
            copy_plugin_distribution_files(&source, output_root, plugin)?;
        }
    }
    Ok(())
}

fn merge_plugin_catalog(
    current_catalog: &Path,
    plugin_release: &Path,
    output_catalog: &Path,
) -> io::Result<()> {
    let incoming_root: serde_json::Value = serde_json::from_slice(&std::fs::read(plugin_release)?)
        .map_err(|error| {
            invalid_data(format!("解析 {} 失败: {error}", plugin_release.display()))
        })?;
    let incoming_plugins = incoming_root
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_data("插件发布目录缺少 plugins"))?;
    if incoming_plugins.len() != 1 {
        return Err(invalid_data("独立插件发布目录必须只包含一个插件"));
    }
    let incoming = incoming_plugins[0].clone();
    let incoming_id = required_json_string(&incoming, "id", plugin_release)?.to_string();
    let incoming_version = required_json_string(&incoming, "version", plugin_release)?.to_string();
    semver::Version::parse(&incoming_version).map_err(|error| {
        invalid_data(format!(
            "插件 {incoming_id} 版本 {incoming_version} 无效: {error}"
        ))
    })?;

    let mut plugins = BTreeMap::<String, serde_json::Value>::new();
    if current_catalog != Path::new("-") && current_catalog.is_file() {
        let current: serde_json::Value = serde_json::from_slice(&std::fs::read(current_catalog)?)
            .map_err(|error| {
            invalid_data(format!("解析 {} 失败: {error}", current_catalog.display()))
        })?;
        if current.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid_data("当前插件目录版本无效"));
        }
        for release in current
            .get("plugins")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid_data("当前插件目录缺少 plugins"))?
        {
            let id = required_json_string(release, "id", current_catalog)?.to_string();
            if plugins.insert(id.clone(), release.clone()).is_some() {
                return Err(invalid_data(format!("当前插件目录包含重复 ID: {id}")));
            }
        }
    }
    plugins.insert(incoming_id.clone(), incoming);
    if let Some(parent) = output_catalog.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_json(
        output_catalog,
        &serde_json::json!({"version": 1, "plugins": plugins.into_values().collect::<Vec<_>>() }),
    )?;
    eprintln!(
        "[xtask] 已将插件 {incoming_id}@{incoming_version} 合并到目录: {}",
        output_catalog.display()
    );
    Ok(())
}
