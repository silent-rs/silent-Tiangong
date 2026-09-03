//! 测试支撑（`#[doc(hidden)]`：非公开 API，仅供本 crate 测试与集成测试
//! 复用）。把 debug Launcher 复制到测试进程目录（launcher_manager 的优先
//! 解析位置）并用测试密钥签名，同时把测试公钥写入 fixture 存储根作为
//! 本机用户密钥信任根——真实链路因此完整经过"启动前验签"。
//!
//! 跨进程并发安全：目标文件内容相同，写入经临时名+原子 rename。

use std::path::{Path, PathBuf};

const TEST_SIGNING_SECRET_KEY: &str = "untrusted comment: rsign encrypted secret key\nRWQAAEIyAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAls4pXFjc5mdI4GLK03g7o1W1/i77lDVt6N0UEBq1kiaRr4Tu8k4qp6+g7ZVZTZVY46bgfgMqxzLtK9cQwyK7Tjh7/3zxhx5Q4jk/sWXImXvRS/pwCH3EFfNivwZFLOJkLCbVcQ2/qz4=";
#[cfg(any(unix, windows))]
const TEST_SIGNING_PUBLIC_KEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDY3RTZEQzU4NUMyOUNFOTYKUldTV3ppbGNXTnptWjYrZzdaVlpUWlZZNDZiZ2ZnTXF4ekx0SzljUXd5SzdUamg3LzN6eGh4NVEK";

/// workspace target/debug 下的二进制路径（不存在返回 None）。
pub fn debug_binary(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let debug_dir = executable.parent()?.parent()?;
    let candidate = debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

/// 准备测试 Launcher：复制+签名+写入信任根。Launcher 未构建时返回
/// 携带指引的 Err（调用方决定跳过或失败）。
pub fn ensure_test_launcher_signed(storage_root: &Path) -> Result<(), String> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = storage_root;
        return Err("当前平台无沙箱链路".to_string());
    }
    #[cfg(any(unix, windows))]
    {
        let launcher_source = debug_binary("tiangong-sandbox").ok_or(
            "target/debug/tiangong-sandbox 尚未构建（先 cargo build -p tiangong-sandbox）",
        )?;
        let test_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .ok_or("无法定位测试进程目录")?;
        let launcher = test_dir.join(format!("tiangong-sandbox{}", std::env::consts::EXE_SUFFIX));
        atomic_copy(&launcher_source, &launcher)?;
        sign_minisign(&launcher)?;
        let keys = storage_root.join("keys");
        std::fs::create_dir_all(&keys).map_err(|e| format!("创建信任根目录失败: {e}"))?;
        // 写入完整密钥对（私钥+公钥）：安装链的本地信任流程会调用
        // ensure_user_signing_key——私钥缺失时生成新密钥对并覆盖公钥，
        // 会顶掉测试公钥导致 Launcher 验签失败；成对写入后走"已存在，
        // 校验配对"分支，测试密钥链保持稳定。
        std::fs::write(keys.join("user-signing.key"), TEST_SIGNING_SECRET_KEY)
            .map_err(|e| format!("写入测试私钥失败: {e}"))?;
        std::fs::write(
            keys.join("user-signing.key.pub"),
            format!("{TEST_SIGNING_PUBLIC_KEY_B64}\n"),
        )
        .map_err(|e| format!("写入测试公钥失败: {e}"))?;
        // 自验：立即验证签名与信任根配对，失败信息携带实际路径，
        // 用于定位 ensure 产物与连接验签路径的分歧。
        let resolved = tiangong_sandbox::launcher_manager::resolve_installed_program(
            &storage_root.join("sandbox"),
        );
        crate::signature::verify_launcher_signature(
            resolved.as_ref().unwrap_or(&launcher),
            storage_root,
        )
        .map_err(|e| {
            format!(
                "测试 Launcher 自验失败（resolved={resolved:?}, launcher={launcher:?}, storage={storage_root:?}）: {e:#}"
            )
        })?;
        Ok(())
    }
}

#[cfg(any(unix, windows))]
fn atomic_copy(source: &Path, destination: &Path) -> Result<(), String> {
    let temp = destination.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::copy(source, &temp).map_err(|e| format!("复制失败: {e}"))?;
    std::fs::rename(&temp, destination).map_err(|e| format!("落位失败: {e}"))
}

#[cfg(any(unix, windows))]
fn sign_minisign(launcher: &Path) -> Result<(), String> {
    let secret_box =
        minisign::SecretKeyBox::from_string(TEST_SIGNING_SECRET_KEY).map_err(|e| e.to_string())?;
    let secret = secret_box
        .into_unencrypted_secret_key()
        .map_err(|e| e.to_string())?;
    let public = minisign::PublicKey::from_secret_key(&secret).map_err(|e| e.to_string())?;
    let data = std::fs::read(launcher).map_err(|e| e.to_string())?;
    let signature = minisign::sign(Some(&public), &secret, data.as_slice(), None, None)
        .map_err(|e| e.to_string())?;
    use base64::Engine;
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.into_string());
    let sig_path = launcher.with_file_name(format!(
        "tiangong-sandbox{}.sig",
        std::env::consts::EXE_SUFFIX
    ));
    let temp = sig_path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, sig_b64).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &sig_path).map_err(|e| e.to_string())
}

/// 当前环境无法应用原生沙箱时的跳过原因（None 表示可用）。
///
/// 天工终端/受限 CI 等外层 Seatbelt 环境内，Launcher 无法再次嵌套应用
/// 沙箱而拒绝启动（退出码 78，安全设计而非缺陷）——真实沙箱链路测试
/// 在此类环境必然失败或卡死，统一跳过并打印原因。
pub fn native_sandbox_skip_reason() -> Option<String> {
    match tiangong_sandbox::sandbox::availability() {
        tiangong_sandbox::SandboxAvailability::Available => None,
        tiangong_sandbox::SandboxAvailability::EnvironmentRestricted(reason)
        | tiangong_sandbox::SandboxAvailability::Unsupported(reason) => Some(reason),
    }
}
