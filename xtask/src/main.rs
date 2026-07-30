//! xtask：辅助构建任务。
//!
//! 提供 `build-wasm` 和 `build-sidecar` 子命令：
//! - build-wasm：构建 memory wasm 组件并部署到 `~/.tiangong/plugins/`
//! - build-sidecar：构建 memory sidecar 二进制并部署到 `~/.tiangong/memory-sidecar/`
//!
//! 用法：
//! ```sh
//! cargo run -p xtask -- build-wasm
//! cargo run -p xtask -- build-sidecar
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

const WASM_CRATE: &str = "tiangong-plugin-memory-wasm";
const WASM_TARGET: &str = "wasm32-wasip2";
const WASM_ARTIFACT: &str = "tiangong_plugin_memory_wasm.wasm";

const SIDECAR_CRATE: &str = "tiangong-memory-sidecar";
const SIDECAR_ARTIFACT: &str = "tiangong-memory-sidecar";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "build-wasm" => {
            if let Err(e) = build_wasm() {
                eprintln!("build-wasm 失败: {e}");
                std::process::exit(1);
            }
        }
        "build-sidecar" => {
            if let Err(e) = build_sidecar() {
                eprintln!("build-sidecar 失败: {e}");
                std::process::exit(1);
            }
        }
        "help" | "--help" | "-h" => print_help(),
        other => {
            eprintln!("未知子命令: {other}");
            print_help();
            std::process::exit(1);
        }
    }
}

fn print_help() {
    eprintln!("xtask - 天工辅助构建任务\n");
    eprintln!("用法: cargo run -p xtask -- <子命令>\n");
    eprintln!("子命令:");
    eprintln!("  build-wasm      构建 memory wasm 组件并部署到 ~/.tiangong/plugins/");
    eprintln!("  build-sidecar   构建 memory sidecar 二进制并部署到 ~/.tiangong/memory-sidecar/");
    eprintln!("  help            显示本帮助");
}

/// 构建 wasm 组件并拷贝到 storage_root/plugins/。
fn build_wasm() -> std::io::Result<()> {
    let workspace_root = workspace_root();
    eprintln!("[xtask] workspace 根目录: {}", workspace_root.display());

    // 1. 构建 wasm 组件（release）。
    eprintln!("[xtask] 构建 {WASM_CRATE}（target={WASM_TARGET}, profile=release）...");
    let status = Command::new(env_var_or("CARGO", "cargo"))
        .current_dir(&workspace_root)
        .args([
            "build",
            "-p",
            WASM_CRATE,
            "--target",
            WASM_TARGET,
            "--release",
        ])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other("cargo build wasm 退出码非零"));
    }

    // 2. 定位构建产物。
    let artifact = workspace_root
        .join("target")
        .join(WASM_TARGET)
        .join("release")
        .join(WASM_ARTIFACT);
    if !artifact.exists() {
        return Err(std::io::Error::other(format!(
            "未找到 wasm 产物: {}",
            artifact.display()
        )));
    }
    eprintln!(
        "[xtask] 构建产物: {} ({} 字节)",
        artifact.display(),
        file_size(&artifact)?
    );

    // 3. 拷贝到 storage_root/plugins/。
    let dest_dir = storage_root().join("plugins");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(WASM_ARTIFACT);
    std::fs::copy(&artifact, &dest)?;
    eprintln!("[xtask] 已部署到: {}", dest.display());

    Ok(())
}

/// 构建 memory sidecar 二进制并部署到 storage_root/memory-sidecar/。
fn build_sidecar() -> std::io::Result<()> {
    let workspace_root = workspace_root();
    eprintln!("[xtask] workspace 根目录: {}", workspace_root.display());

    // 1. 构建 sidecar 二进制（release）。
    eprintln!("[xtask] 构建 {SIDECAR_CRATE}（profile=release）...");
    let status = Command::new(env_var_or("CARGO", "cargo"))
        .current_dir(&workspace_root)
        .args(["build", "-p", SIDECAR_CRATE, "--release"])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other("cargo build sidecar 退出码非零"));
    }

    // 2. 定位构建产物（带平台可执行后缀）。
    let exe = std::env::consts::EXE_SUFFIX;
    let artifact = workspace_root
        .join("target")
        .join("release")
        .join(format!("{SIDECAR_ARTIFACT}{exe}"));
    if !artifact.exists() {
        return Err(std::io::Error::other(format!(
            "未找到 sidecar 产物: {}",
            artifact.display()
        )));
    }
    eprintln!(
        "[xtask] 构建产物: {} ({} 字节)",
        artifact.display(),
        file_size(&artifact)?
    );

    // 3. 拷贝到 storage_root/memory-sidecar/。
    let dest_dir = storage_root().join("memory-sidecar");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(format!("{SIDECAR_ARTIFACT}{exe}"));
    std::fs::copy(&artifact, &dest)?;

    // Unix 设置可执行权限。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("[xtask] 已部署到: {}", dest.display());

    Ok(())
}

/// workspace 根目录：xtask crate 在 workspace 根下的 xtask/，上溯一级。
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// 天工存储根目录（`~/.tiangong`），与 tiangong-config::io::storage_root 一致。
fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// 用户主目录（兼容 HOME / USERPROFILE / HOMEDRIVE+HOMEPATH）。
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}

fn env_var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn file_size(path: &Path) -> std::io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}
