//! 插件签名信任根：官方（内置公钥）、本机用户密钥对、导入的第三方公钥。
//!
//! 三类信任根共用 `signature.rs` 的同一条验证路径，本模块只负责「发布者
//! 标识 → 验签公钥」的路由与信任根自身的管理。存储布局（storage_root 下）：
//!
//! ```text
//! keys/user-signing.key          用户私钥（minisign 文本，权限 0600，未加密）
//! keys/user-signing.key.pub      用户公钥（base64(公钥文本)，同 tauri signer）
//! keys/trusted-publishers.json   第三方公钥登记表
//! ```
//!
//! 安全边界：第三方公钥导入与移除只应由宿主设置界面（用户手动操作）触发，
//! 本模块不做交互决策；调用方必须保证不向插件代码或 Agent 工具暴露这些
//! 管理接口。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 官方发布者标识（内置公钥验签）。
pub const OFFICIAL_PUBLISHER: &str = "tiangong-official";
/// 本机用户密钥的发布者标识（创作链自动签名使用）。
pub const LOCAL_PUBLISHER: &str = "local";

const USER_SIGNING_KEY_FILE: &str = "user-signing.key";
const TRUSTED_PUBLISHERS_FILE: &str = "trusted-publishers.json";

/// 信任存储串行锁：用户密钥生成与登记表读写共用——防并发导入/移除丢失
/// 更新、防密钥对双生成竞态（私钥 A 与公钥 B 混搭）。
static TRUST_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 临时文件 + 原子改名落盘：写一半崩溃不会留下半截密钥/登记表。
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", scru128::new()));
    std::fs::write(&temporary, content)
        .with_context(|| format!("写入临时文件失败: {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("原子落盘失败: {}", path.display()))?;
    Ok(())
}

/// 已导入的第三方信任根条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPublisher {
    pub publisher: String,
    /// base64(minisign 公钥文本)——与公钥文件内容同格式。
    pub public_key_b64: String,
    /// 公钥指纹（sha256 前 16 字符 hex），导入界面展示与人工比对用。
    pub fingerprint: String,
    /// 导入时间（本地时间）。
    pub imported_at: String,
}

/// 插件数据存储根（keys 目录所在）。已安装插件与暂存目录均为
/// `storage_root/<目录>/<id>` 两级布局。
pub fn storage_root_of(directory: &Path) -> Result<&Path> {
    let mut current = Some(directory);
    while let Some(directory) = current {
        if directory.file_name().is_some_and(|name| name == "plugins")
            && directory.parent().is_some()
        {
            return directory
                .parent()
                .context("无法从插件目录推导存储根（plugins 目录缺少父目录）");
        }
        current = directory.parent();
    }
    bail!(
        "无法从插件目录推导存储根（未找到 plugins 父目录）: {}",
        directory.display()
    )
}

fn keys_root(storage_root: &Path) -> PathBuf {
    storage_root.join("keys")
}

fn user_key_path(storage_root: &Path) -> PathBuf {
    keys_root(storage_root).join(USER_SIGNING_KEY_FILE)
}

/// 计算公钥指纹：sha256(公钥文本) 前 16 字符 hex。
pub fn publisher_fingerprint(public_key_b64: &str) -> Result<String> {
    let public_text = decode_public_key_text(public_key_b64)?;
    Ok(hex::encode(Sha256::digest(public_text.as_bytes()))[..16].to_string())
}

/// 校验公钥环境格式（base64(公钥文本)）并返回归一化（trim 后）的 b64。
fn normalize_public_key_b64(public_key_b64: &str) -> Result<String> {
    decode_public_key_text(public_key_b64)?;
    Ok(public_key_b64.trim().to_string())
}

/// 校验并解码公钥环境格式（base64(公钥文本) → 公钥文本），同时验证公钥
/// 本身可被 minisign 解析。
fn decode_public_key_text(public_key_b64: &str) -> Result<String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64.trim())
        .context("公钥不是有效的 base64 编码")?;
    let text = String::from_utf8(decoded).context("公钥 base64 内容不是有效 UTF-8")?;
    if !text.trim().starts_with("untrusted comment") {
        bail!("公钥内容无效（缺少 minisign 注释头）");
    }
    minisign::PublicKeyBox::from_string(text.trim()).context("公钥不是有效的 minisign 格式")?;
    Ok(text.trim().to_string())
}

/// 确保用户签名密钥存在（不存在则生成），返回私钥路径。
///
/// 密钥为未加密 minisign 格式——创作链要求免交互签名；密钥文件权限收紧
/// 到仅当前用户可读写（Unix 0600）。
pub fn ensure_user_signing_key(storage_root: &Path) -> Result<PathBuf> {
    let key_path = user_key_path(storage_root);
    if key_path.is_file() {
        return Ok(key_path);
    }
    let _guard = TRUST_STORE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("信任存储锁已损坏"))?;
    if key_path.is_file() {
        return Ok(key_path);
    }
    std::fs::create_dir_all(keys_root(storage_root))
        .with_context(|| format!("创建密钥目录失败: {}", keys_root(storage_root).display()))?;
    let keypair = minisign::KeyPair::generate_unencrypted_keypair().context("生成用户密钥失败")?;
    let secret_text = keypair
        .sk
        .to_box(None)
        .context("序列化用户私钥失败")?
        .into_string();
    atomic_write(&key_path, secret_text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("收紧私钥权限失败: {}", key_path.display()))?;
    }
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(
        keypair
            .pk
            .to_box()
            .context("序列化用户公钥失败")?
            .into_string(),
    );
    atomic_write(
        &public_key_path(storage_root),
        format!("{public_b64}\n").as_bytes(),
    )?;
    tracing::info!(key = %key_path.display(), "已生成插件用户签名密钥");
    Ok(key_path)
}

/// 用户公钥路径。
pub fn public_key_path(storage_root: &Path) -> PathBuf {
    let mut path = user_key_path(storage_root).into_os_string();
    path.push(".pub");
    PathBuf::from(path)
}

/// 读取用户公钥（base64 格式）。密钥未生成时返回错误（调用方先 ensure）。
pub fn user_public_key_b64(storage_root: &Path) -> Result<String> {
    let path = public_key_path(storage_root);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("读取用户公钥失败（尚未生成？）: {}", path.display()))?;
    normalize_public_key_b64(&raw)
}

/// 导入第三方公钥：登记、计算指纹；发布者已存在（或与保留标识冲突）时拒绝。
pub fn import_trusted_publisher(
    storage_root: &Path,
    publisher: &str,
    public_key_b64: &str,
) -> Result<TrustedPublisher> {
    validate_publisher_id(publisher)?;
    let public_key_b64 = normalize_public_key_b64(public_key_b64)?;
    let _guard = TRUST_STORE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("信任存储锁已损坏"))?;
    let mut publishers = load_trusted_publishers(storage_root)?;
    if publishers.iter().any(|entry| entry.publisher == publisher) {
        bail!("发布者 {publisher} 已存在；如需更换请先移除旧公钥");
    }
    let entry = TrustedPublisher {
        publisher: publisher.to_string(),
        fingerprint: publisher_fingerprint(&public_key_b64)?,
        public_key_b64,
        imported_at: chrono::Local::now().naive_local().to_string(),
    };
    publishers.push(entry.clone());
    publishers.sort_by(|left, right| left.publisher.cmp(&right.publisher));
    save_trusted_publishers(storage_root, &publishers)?;
    tracing::info!(publisher = %entry.publisher, %entry.fingerprint, "已导入第三方插件公钥");
    Ok(entry)
}

/// 移除第三方公钥；返回是否确实移除。
pub fn remove_trusted_publisher(storage_root: &Path, publisher: &str) -> Result<bool> {
    let _guard = TRUST_STORE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("信任存储锁已损坏"))?;
    let mut publishers = load_trusted_publishers(storage_root)?;
    let before = publishers.len();
    publishers.retain(|entry| entry.publisher != publisher);
    let removed = publishers.len() != before;
    if removed {
        save_trusted_publishers(storage_root, &publishers)?;
        tracing::info!(publisher, "已移除第三方插件公钥");
    }
    Ok(removed)
}

/// 列出已导入的第三方信任根。
pub fn list_trusted_publishers(storage_root: &Path) -> Result<Vec<TrustedPublisher>> {
    load_trusted_publishers(storage_root)
}

fn load_trusted_publishers(storage_root: &Path) -> Result<Vec<TrustedPublisher>> {
    let path = keys_root(storage_root).join(TRUSTED_PUBLISHERS_FILE);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("读取第三方公钥登记表失败: {}", path.display()))?;
    let publishers: Vec<TrustedPublisher> = serde_json::from_str(&raw)
        .with_context(|| format!("解析第三方公钥登记表失败: {}", path.display()))?;
    Ok(publishers)
}

fn save_trusted_publishers(storage_root: &Path, publishers: &[TrustedPublisher]) -> Result<()> {
    let path = keys_root(storage_root).join(TRUSTED_PUBLISHERS_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(publishers)?)
        .with_context(|| format!("写入第三方公钥登记表失败: {}", path.display()))?;
    Ok(())
}

/// 发布者 → 验签公钥（base64 格式）路由。
///
/// 保留标识：`tiangong-official` 与 `local`；其余一律查第三方登记表，
/// 未导入时返回带指引的错误。环境变量覆盖仅作用于官方形态（既有测试
/// 通道，不改变多信任根路由）。
/// 官方信任公钥（base64）：环境变量覆盖（CI/本地端到端验证通道）优先，
/// 否则内置官方公钥。
pub fn official_pubkey_b64() -> String {
    std::env::var("TIANGONG_PLUGIN_PUBKEY_B64")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::signature::OFFICIAL_PUBKEY_B64.to_string())
}

pub fn resolve_publisher_pubkey(storage_root: &Path, publisher: &str) -> Result<String> {
    match publisher {
        OFFICIAL_PUBLISHER => Ok(official_pubkey_b64()),
        LOCAL_PUBLISHER => user_public_key_b64(storage_root),
        other => {
            validate_publisher_id(other)?;
            let publishers = load_trusted_publishers(storage_root)?;
            publishers
                .iter()
                .find(|entry| entry.publisher == other)
                .map(|entry| entry.public_key_b64.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "发布者 {other} 的公钥未导入；请先在设置的插件信任管理中导入该开发者的公钥"
                    )
                })
        }
    }
}

/// 发布者标识格式：非空、不超过 64 字符、ASCII 字母数字与 `-` `_` `.`，
/// 不得为保留标识。
pub fn validate_publisher_id(publisher: &str) -> Result<()> {
    if publisher.is_empty()
        || publisher.len() > 64
        || !publisher
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || publisher.starts_with('.')
        || publisher.ends_with('.')
        || publisher == OFFICIAL_PUBLISHER
        || publisher == LOCAL_PUBLISHER
    {
        bail!("发布者标识无效: {publisher}（字母数字与 - _ .，1-64 字符，且不得使用保留标识）");
    }
    Ok(())
}

/// 用用户私钥对文件签名，落盘「签名文本整体 base64」的 `.sig`（与官方
/// 发布链同格式）。双路径加载私钥：用户密钥恒为未加密，仍保留加密回退
/// 以兼容手工替换的密钥文件。
pub fn sign_with_user_key(storage_root: &Path, content_path: &Path) -> Result<()> {
    let key_path = ensure_user_signing_key(storage_root)?;
    let key_text = std::fs::read_to_string(&key_path)
        .with_context(|| format!("读取用户私钥失败: {}", key_path.display()))?;
    let secret_key = minisign::SecretKeyBox::from_string(key_text.trim())
        .and_then(|secret_key_box| secret_key_box.into_unencrypted_secret_key())
        .or_else(|_| {
            minisign::SecretKeyBox::from_string(key_text.trim())?
                .into_secret_key(Some(String::new()))
        })
        .context("加载用户私钥失败")?;
    let public_key =
        minisign::PublicKey::from_secret_key(&secret_key).context("推导用户公钥失败")?;
    let content = std::fs::read(content_path)
        .with_context(|| format!("读取待签名内容失败: {}", content_path.display()))?;
    let signature = minisign::sign(
        Some(&public_key),
        &secret_key,
        content.as_slice(),
        None,
        None,
    )
    .context("用户密钥签名失败")?;
    let signature_b64 = base64::engine::general_purpose::STANDARD.encode(signature.into_string());
    let mut signature_path = content_path.as_os_str().to_os_string();
    signature_path.push(".sig");
    std::fs::write(PathBuf::from(signature_path), signature_b64).context("写入用户签名失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn 用户密钥_生成与公钥读取闭环() {
        let root = storage();
        ensure_user_signing_key(root.path()).unwrap();
        // 幂等：再次 ensure 不重复生成（私钥 create_new 保证）。
        ensure_user_signing_key(root.path()).unwrap();
        let public_b64 = user_public_key_b64(root.path()).unwrap();
        assert!(publisher_fingerprint(&public_b64).unwrap().len() == 16);
    }

    #[test]
    fn 用户密钥_签名与验签闭环() {
        let root = storage();
        let content = root.path().join("release.json");
        std::fs::write(&content, br#"{"a":1}"#).unwrap();
        sign_with_user_key(root.path(), &content).unwrap();
        assert!(content.with_file_name("release.json.sig").is_file());

        // 公钥验证签名（与运行时 verify_minisign 相同格式链）。
        let public_b64 = user_public_key_b64(root.path()).unwrap();
        let public_text = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(public_b64.trim())
                .unwrap(),
        )
        .unwrap();
        let public_key = minisign::PublicKeyBox::from_string(public_text.trim())
            .unwrap()
            .into_public_key()
            .unwrap();
        let signature_raw = std::fs::read_to_string(root.path().join("release.json.sig")).unwrap();
        let signature_text = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(signature_raw.trim())
                .unwrap(),
        )
        .unwrap();
        let signature_box = minisign::SignatureBox::from_string(signature_text.trim()).unwrap();
        let data = std::fs::read(&content).unwrap();
        let mut reader = std::io::Cursor::new(data);
        minisign::verify(
            &public_key,
            &signature_box,
            &mut reader,
            false,
            false,
            false,
        )
        .unwrap();
    }

    #[test]
    fn 三方公钥_导入指纹移除与路由() {
        let root = storage();
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let public_b64 = base64::engine::general_purpose::STANDARD
            .encode(keypair.pk.to_box().unwrap().into_string());

        let entry = import_trusted_publisher(root.path(), "acme-dev", &public_b64).unwrap();
        assert_eq!(entry.fingerprint.len(), 16);
        assert_eq!(list_trusted_publishers(root.path()).unwrap().len(), 1);

        // 重复导入拒绝。
        assert!(import_trusted_publisher(root.path(), "acme-dev", &public_b64).is_err());
        // 保留标识拒绝。
        assert!(import_trusted_publisher(root.path(), LOCAL_PUBLISHER, &public_b64).is_err());
        assert!(import_trusted_publisher(root.path(), OFFICIAL_PUBLISHER, &public_b64).is_err());
        // 无效公钥拒绝。
        assert!(import_trusted_publisher(root.path(), "bad-key", "not-base64!!").is_err());

        // 路由命中。
        assert_eq!(
            resolve_publisher_pubkey(root.path(), "acme-dev").unwrap(),
            public_b64.trim()
        );
        // 未导入发布者给出指引。
        let error = resolve_publisher_pubkey(root.path(), "unknown-dev").unwrap_err();
        assert!(format!("{error:#}").contains("未导入"), "{error:#}");

        // 移除后失效。
        assert!(remove_trusted_publisher(root.path(), "acme-dev").unwrap());
        assert!(resolve_publisher_pubkey(root.path(), "acme-dev").is_err());
        assert!(!remove_trusted_publisher(root.path(), "acme-dev").unwrap());
    }

    #[test]
    fn 路由_本地发布者走用户公钥() {
        let root = storage();
        // 未生成密钥时明确失败（带指引）。
        assert!(resolve_publisher_pubkey(root.path(), LOCAL_PUBLISHER).is_err());
        ensure_user_signing_key(root.path()).unwrap();
        assert!(resolve_publisher_pubkey(root.path(), LOCAL_PUBLISHER).is_ok());
    }
}
