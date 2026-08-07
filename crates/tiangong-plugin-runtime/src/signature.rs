//! 插件发布清单数字签名与制品完整性验证。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, PluginManifest};

pub const SIGNED_RELEASE_FILE: &str = "release.json";
pub const SIGNATURE_FILE: &str = "release.json.sig";
pub const SIGNED_RELEASE_SCHEMA_VERSION: u32 = 1;

/// 插件专用 minisign 公钥。私钥只保存在官方发布环境。
const OFFICIAL_PLUGIN_PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDkwQzBDOEJEQ0IzRTI5OTgKUldTWUtUN0x2Y2pBa0piU3JNQi9VRDlENVdxNzd6S3Z1MGo1ck5Sd2ZwNTRKTnpVTGkyWjE5dGMK";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPluginRelease {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub publisher: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub manifest: SignedArtifact,
    pub wasm: SignedArtifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SignedArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedArtifact {
    pub path: PathBuf,
    pub sha256: String,
}

impl SignedPluginRelease {
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|item| item == permission)
    }
}

/// 验证签名发布清单及其覆盖的当前平台制品。
///
/// 缺少签名文件返回 `Ok(None)`，表示普通未签名插件；任何签名或制品不一致均返回错误。
pub fn verify_signed_release(
    directory: &Path,
    plugin_manifest: &PluginManifest,
) -> Result<Option<SignedPluginRelease>> {
    let release_path = directory.join(SIGNED_RELEASE_FILE);
    let signature_path = directory.join(SIGNATURE_FILE);
    let release_exists = release_path.is_file();
    let signature_exists = signature_path.is_file();
    if !release_exists && !signature_exists {
        return Ok(None);
    }
    if !release_exists || !signature_exists {
        bail!("插件签名文件不完整");
    }

    let release_bytes = std::fs::read(&release_path)
        .with_context(|| format!("读取插件签名清单失败: {}", release_path.display()))?;
    verify_minisign(&release_bytes, &std::fs::read_to_string(&signature_path)?)?;
    let release: SignedPluginRelease =
        serde_json::from_slice(&release_bytes).context("解析插件签名清单失败")?;
    release.validate(directory, plugin_manifest)?;
    Ok(Some(release))
}

impl SignedPluginRelease {
    fn validate(&self, directory: &Path, plugin_manifest: &PluginManifest) -> Result<()> {
        if self.schema_version != SIGNED_RELEASE_SCHEMA_VERSION {
            bail!("插件签名清单版本不支持: {}", self.schema_version);
        }
        if self.publisher != "tiangong-official" {
            bail!("插件签名发布者无效: {}", self.publisher);
        }
        if self.id != plugin_manifest.id || self.version != plugin_manifest.version {
            bail!("插件签名清单与 plugin.json 的 ID 或版本不一致");
        }
        if self.permissions.iter().any(|item| item.trim().is_empty())
            || permission_set(&self.permissions)?.len() != self.permissions.len()
        {
            bail!("插件签名清单包含空权限或重复权限");
        }
        if plugin_manifest
            .permissions
            .iter()
            .any(|item| item.trim().is_empty())
            || permission_set(&plugin_manifest.permissions)?.len()
                != plugin_manifest.permissions.len()
        {
            bail!("plugin.json 包含空权限或重复权限");
        }
        if permission_set(&self.permissions)? != permission_set(&plugin_manifest.permissions)? {
            bail!("插件签名清单与 plugin.json 的权限声明不一致");
        }
        self.manifest.verify(directory, Path::new(MANIFEST_FILE))?;
        self.wasm.verify(directory, plugin_manifest.wasm_binary())?;
        match (&self.sidecar, &plugin_manifest.sidecar) {
            (Some(signed), Some(sidecar)) => {
                if !self.has_permission("sidecar.invoke") {
                    bail!("插件签名清单未授权 sidecar.invoke");
                }
                signed.verify(directory, &sidecar_binary_path(&sidecar.binary)?)?;
            }
            (None, None) => {}
            _ => bail!("插件签名清单与 plugin.json 的 sidecar 声明不一致"),
        }
        Ok(())
    }
}

impl SignedArtifact {
    fn verify(&self, directory: &Path, expected_path: &Path) -> Result<()> {
        validate_relative_path(&self.path)?;
        if self.path != expected_path {
            bail!(
                "签名制品路径不一致: expected={}, actual={}",
                expected_path.display(),
                self.path.display()
            );
        }
        let actual = sha256_file(&directory.join(&self.path))?;
        if !actual.eq_ignore_ascii_case(self.sha256.trim_start_matches("sha256:")) {
            bail!("签名制品校验失败: {}", self.path.display());
        }
        Ok(())
    }
}

fn permission_set(permissions: &[String]) -> Result<BTreeSet<&str>> {
    Ok(permissions.iter().map(String::as_str).collect())
}

fn verify_minisign(content: &[u8], signature_b64: &str) -> Result<()> {
    let public_text = base64::engine::general_purpose::STANDARD
        .decode(OFFICIAL_PLUGIN_PUBKEY_B64)
        .context("解析内置插件公钥失败")?;
    let public_text = String::from_utf8(public_text).context("内置插件公钥非 UTF-8")?;
    let public_key = PublicKey::decode(&public_text).context("解析插件公钥失败")?;
    let signature_text = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .context("解析插件签名失败")?;
    let signature_text = String::from_utf8(signature_text).context("插件签名非 UTF-8")?;
    let signature = Signature::decode(&signature_text).context("解析 minisign 插件签名失败")?;
    public_key
        .verify(content, &signature, false)
        .context("插件签名验证不通过")
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("读取签名制品失败: {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("签名制品路径必须是安全相对路径");
    }
    Ok(())
}

fn sidecar_binary_path(path: &Path) -> Result<PathBuf> {
    let mut path = path.to_path_buf();
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("sidecar 文件名无效")?;
        if !name.ends_with(std::env::consts::EXE_SUFFIX) {
            path.set_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        }
    }
    Ok(path)
}
