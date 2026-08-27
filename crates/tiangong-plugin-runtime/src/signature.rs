//! 插件发布清单数字签名与制品完整性验证。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, PluginManifest, SidecarRuntime};
use crate::sidecar::CONTENT_MANIFEST_FILE;

pub const SIGNED_RELEASE_FILE: &str = "release.json";
pub const SIGNATURE_FILE: &str = "release.json.sig";
pub const SIGNED_RELEASE_SCHEMA_VERSION: u32 = 1;

/// 插件专用 minisign 公钥。私钥只保存在官方发布环境（CI secret 与
/// 官方开发者本机，打包时经 TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH 提供）。
pub const OFFICIAL_PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDkwQzBDOEJEQ0IzRTI5OTgKUldTWUtUN0x2Y2pBa0piU3JNQi9VRDlENVdxNzd6S3Z1MGo1ck5Sd2ZwNTRKTnpVTGkyWjE5dGMK";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPluginRelease {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub publisher: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub manifest: SignedArtifact,
    /// 纯 TS/sidecar 插件无 wasm 制品，签名清单省略 wasm 条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<SignedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui: Vec<SignedArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SignedArtifact>,
    /// 解释器 sidecar 插件：官方签名锚定完整内容清单（content-manifest.json
    /// 的哈希），清单覆盖全树——与本地信任锚同构，签名即信任根。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_manifest: Option<SignedArtifact>,
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
    // 先解析拿到发布者标识，再按信任根路由验签（官方内置 / 本机用户
    // 密钥 / 已导入第三方公钥——见 trust.rs）。
    let release: SignedPluginRelease =
        serde_json::from_slice(&release_bytes).context("解析插件签名清单失败")?;
    verify_minisign_for_publisher(
        &release.publisher,
        directory,
        &release_bytes,
        &std::fs::read_to_string(&signature_path)?,
    )?;
    release.validate(directory, plugin_manifest)?;
    Ok(Some(release))
}

impl SignedPluginRelease {
    fn validate(&self, directory: &Path, plugin_manifest: &PluginManifest) -> Result<()> {
        if self.schema_version != SIGNED_RELEASE_SCHEMA_VERSION {
            bail!("插件签名清单版本不支持: {}", self.schema_version);
        }
        // 发布者格式校验（合法标识由路由侧解析为对应信任根；保留标识
        // 与三方标识均在 trust.rs 路由，未导入即验签失败）。
        if self.publisher != crate::trust::OFFICIAL_PUBLISHER
            && self.publisher != crate::trust::LOCAL_PUBLISHER
        {
            crate::trust::validate_publisher_id(&self.publisher)
                .with_context(|| "插件签名发布者无效")?;
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
        // wasm 条目与 plugin.json 声明一致才校验；纯 TS/sidecar 插件两边都缺省。
        match (&self.wasm, plugin_manifest.wasm_binary()) {
            (Some(signed_wasm), Some(wasm_binary)) => signed_wasm.verify(directory, wasm_binary)?,
            (None, None) => {}
            _ => bail!("插件签名清单与 plugin.json 的 wasm 声明不一致"),
        }
        let expected_ui = plugin_manifest
            .ui_contributions()
            .into_iter()
            .map(|contribution| PathBuf::from(contribution.entry))
            .collect::<BTreeSet<_>>();
        let signed_ui = self
            .ui
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect::<BTreeSet<_>>();
        if signed_ui.len() != self.ui.len() || signed_ui != expected_ui {
            bail!("插件签名清单与 plugin.json 的 UI 入口声明不一致");
        }
        for artifact in &self.ui {
            artifact.verify(directory, &artifact.path)?;
        }
        // 插件声明 sidecar 时，官方签名必须同时授权调用（两种形态一致）。
        if plugin_manifest.sidecar.is_some() && !self.has_permission("sidecar.invoke") {
            bail!("插件签名清单未授权 sidecar.invoke");
        }
        match (&self.sidecar, &plugin_manifest.sidecar) {
            (Some(signed), Some(sidecar)) => match sidecar.runtime {
                SidecarRuntime::Native => {
                    if self.content_manifest.is_some() {
                        bail!("native sidecar 签名清单不得包含 content_manifest 条目");
                    }
                    let binary = sidecar.binary.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "插件 {} native sidecar 缺少 binary 声明",
                            plugin_manifest.id
                        )
                    })?;
                    signed.verify(directory, &sidecar_binary_path(binary)?)?;
                }
                SidecarRuntime::Node | SidecarRuntime::Python => {
                    bail!(
                        "解释器 sidecar 签名清单不得包含二进制条目（信任由 content_manifest 承载）"
                    );
                }
            },
            (None, None) => {}
            // 解释器官方形态：签名无二进制条目，信任锚是完整内容清单——
            // 先校验清单文件哈希与签名一致，再按清单双向校验全树。
            (None, Some(sidecar)) if sidecar.runtime != SidecarRuntime::Native => {
                let signed_manifest = self.content_manifest.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("官方解释器插件缺少 content_manifest 签名条目")
                })?;
                signed_manifest.verify(directory, Path::new(CONTENT_MANIFEST_FILE))?;
                crate::sidecar::SidecarConfig::verify_integrity_manifest(
                    &directory.join(CONTENT_MANIFEST_FILE),
                    directory,
                )?;
            }
            _ => bail!("插件签名清单与 plugin.json 的 sidecar 声明不一致"),
        }
        // 内容清单条目只属于解释器形态：无 sidecar 声明或 native 形态携带该条目
        // 均不符合发布约定。
        if self.content_manifest.is_some()
            && plugin_manifest
                .sidecar
                .as_ref()
                .is_none_or(|sidecar| sidecar.runtime == SidecarRuntime::Native)
        {
            bail!("content_manifest 签名条目仅允许解释器 sidecar 形态使用");
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

/// 按发布者路由信任根验签（官方内置 / 本机用户密钥 / 已导入第三方公钥）。
/// 官方信任根唯一且不可配置；本机与三方信任根按插件目录推导存储位置。
fn verify_minisign_for_publisher(
    publisher: &str,
    directory: &Path,
    content: &[u8],
    signature_b64: &str,
) -> Result<()> {
    // 官方形态直用内置公钥（唯一信任根，不依赖本地存储布局）。
    let pubkey_b64 = if publisher == crate::trust::OFFICIAL_PUBLISHER {
        crate::signature::OFFICIAL_PUBKEY_B64.to_string()
    } else {
        let storage_root = crate::trust::storage_root_of(directory)?;
        crate::trust::resolve_publisher_pubkey(storage_root, publisher)?
    };
    verify_minisign_with_pubkey(content, signature_b64, &pubkey_b64)
}

/// 以显式公钥验签（不读任何全局状态；测试与路由层共用）。
fn verify_minisign_with_pubkey(
    content: &[u8],
    signature_b64: &str,
    pubkey_b64: &str,
) -> Result<()> {
    let public_text = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64.trim())
        .context("解析插件信任公钥失败")?;
    let public_text = String::from_utf8(public_text).context("插件信任公钥非 UTF-8")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::PluginManifest;

    /// devkit 同款内容清单：受管全树（排除清单自身与签名文件）路径 + sha256。
    fn write_content_manifest(dir: &Path) {
        let mut files = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                let path = entry.path();
                let relative = path.strip_prefix(dir).unwrap().display().to_string();
                if relative == CONTENT_MANIFEST_FILE
                    || relative == SIGNED_RELEASE_FILE
                    || relative == SIGNATURE_FILE
                {
                    continue;
                }
                let raw = std::fs::read(&path).unwrap();
                files.push(serde_json::json!({
                    "path": relative.replace('\\', "/"),
                    "sha256": hex::encode(Sha256::digest(&raw)),
                }));
            }
        }
        std::fs::write(
            dir.join(CONTENT_MANIFEST_FILE),
            serde_json::to_vec(&serde_json::json!({"algorithm": "sha256", "files": files}))
                .unwrap(),
        )
        .unwrap();
    }

    /// 官方解释器插件目录：plugin.json + UI 入口 + node sidecar 入口 + 模板
    /// 资源 + 内容清单。
    fn setup_official_interpreter_plugin(root: &Path) -> (PathBuf, PluginManifest) {
        let dir = root.join("plugins").join("official-demo");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::create_dir_all(dir.join("sidecar")).unwrap();
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"official-demo","version":"0.2.0","permissions":["sidecar.invoke"],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]},"sidecar":{"runtime":"node","entry":"sidecar/main.mjs"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html>demo</html>").unwrap();
        std::fs::write(dir.join("sidecar/main.mjs"), "// sidecar bundle").unwrap();
        std::fs::write(dir.join("templates/node-tool.txt"), "template").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).expect("清单合法");
        (dir, manifest)
    }

    fn artifact(dir: &Path, path: &str) -> SignedArtifact {
        SignedArtifact {
            path: PathBuf::from(path),
            sha256: sha256_file(&dir.join(path)).unwrap(),
        }
    }

    /// 生成密钥对、按指定发布者构造签名清单并落盘签名；返回测试公钥
    /// （base64 格式，供导入第三方信任根或做冒充官方的负面测试）。
    fn sign_release_for_publisher(
        dir: &Path,
        manifest: &PluginManifest,
        publisher: &str,
        mutate: impl FnOnce(&mut SignedPluginRelease),
    ) -> String {
        let mut release = SignedPluginRelease {
            schema_version: SIGNED_RELEASE_SCHEMA_VERSION,
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            publisher: publisher.to_string(),
            permissions: manifest.permissions.clone(),
            manifest: artifact(dir, MANIFEST_FILE),
            wasm: None,
            ui: manifest
                .ui_contributions()
                .into_iter()
                .map(|contribution| artifact(dir, &contribution.entry))
                .collect(),
            sidecar: None,
            content_manifest: Some(artifact(dir, CONTENT_MANIFEST_FILE)),
        };
        mutate(&mut release);
        let bytes = serde_json::to_vec_pretty(&release).unwrap();
        std::fs::write(dir.join(SIGNED_RELEASE_FILE), &bytes).unwrap();
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature =
            minisign::sign(Some(&keypair.pk), &keypair.sk, bytes.as_slice(), None, None).unwrap();
        // 与 tauri signer 输出一致：两行签名文本整体 base64 包装（verify_minisign 读取格式）。
        let signature_text = signature.into_string();
        std::fs::write(
            dir.join(SIGNATURE_FILE),
            base64::engine::general_purpose::STANDARD.encode(signature_text),
        )
        .unwrap();
        base64::engine::general_purpose::STANDARD.encode(keypair.pk.to_box().unwrap().into_string())
    }

    fn assert_verify_ok(dir: &Path, manifest: &PluginManifest) {
        verify_signed_release(dir, manifest).expect("官方签名验证应通过");
    }

    fn assert_verify_err(dir: &Path, manifest: &PluginManifest, needle: &str) {
        let error = verify_signed_release(dir, manifest).expect_err("官方签名验证应被拒绝");
        assert!(error.to_string().contains(needle), "{error:#}");
    }

    #[test]
    #[serial_test::serial]
    fn 官方信任根不可替换_测试密钥冒充官方被拒() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        // 测试密钥签署 publisher=tiangong-official 的清单（签名格式合法、
        // 内容清单完备）：官方信任根是内置公钥，必须拒绝。
        let key = sign_release_for_publisher(&dir, &manifest, "tiangong-official", |_| {});
        assert!(
            verify_signed_release(&dir, &manifest).is_err(),
            "测试密钥不得被识别为官方密钥"
        );
        // 环境残留同名变量不影响结果（运行时已完全忽略该变量——此处保留
        // 攻击形态断言，证明覆盖通道已不存在）。
        let previous = std::env::var("TIANGONG_PLUGIN_PUBKEY_B64").ok();
        unsafe {
            std::env::set_var("TIANGONG_PLUGIN_PUBKEY_B64", &key);
        }
        let still_rejected = verify_signed_release(&dir, &manifest).is_err();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("TIANGONG_PLUGIN_PUBKEY_B64", value),
                None => std::env::remove_var("TIANGONG_PLUGIN_PUBKEY_B64"),
            }
        }
        assert!(still_rejected, "官方信任根不得被环境变量替换");
    }

    #[test]
    #[serial_test::serial]
    fn 本机用户签名_验签通过并覆盖全树() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        // 创作链同款：用户密钥自动生成 + local 发布者签名清单。
        crate::trust::ensure_user_signing_key(root.path()).unwrap();
        let mut release = SignedPluginRelease {
            schema_version: SIGNED_RELEASE_SCHEMA_VERSION,
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            publisher: crate::trust::LOCAL_PUBLISHER.to_string(),
            permissions: manifest.permissions.clone(),
            manifest: artifact(&dir, MANIFEST_FILE),
            wasm: None,
            ui: manifest
                .ui_contributions()
                .into_iter()
                .map(|contribution| artifact(&dir, &contribution.entry))
                .collect(),
            sidecar: None,
            content_manifest: Some(artifact(&dir, CONTENT_MANIFEST_FILE)),
        };
        release.manifest = artifact(&dir, MANIFEST_FILE);
        let bytes = serde_json::to_vec_pretty(&release).unwrap();
        std::fs::write(dir.join(SIGNED_RELEASE_FILE), &bytes).unwrap();
        crate::trust::sign_with_user_key(root.path(), &dir.join(SIGNED_RELEASE_FILE)).unwrap();
        verify_signed_release(&dir, &manifest)
            .expect("用户签名（local 发布者）应验签通过")
            .expect("应返回签名清单");
        // 篡改 sidecar 后用户签名同样拒绝（全树校验与官方一致）。
        std::fs::write(dir.join("sidecar/main.mjs"), "// tampered").unwrap();
        assert!(verify_signed_release(&dir, &manifest).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn 三方发布者_导入公钥后验签_未导入拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let release = SignedPluginRelease {
            schema_version: SIGNED_RELEASE_SCHEMA_VERSION,
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            publisher: "acme-dev".to_string(),
            permissions: manifest.permissions.clone(),
            manifest: artifact(&dir, MANIFEST_FILE),
            wasm: None,
            ui: manifest
                .ui_contributions()
                .into_iter()
                .map(|contribution| artifact(&dir, &contribution.entry))
                .collect(),
            sidecar: None,
            content_manifest: Some(artifact(&dir, CONTENT_MANIFEST_FILE)),
        };
        let bytes = serde_json::to_vec_pretty(&release).unwrap();
        std::fs::write(dir.join(SIGNED_RELEASE_FILE), &bytes).unwrap();
        let signature =
            minisign::sign(Some(&keypair.pk), &keypair.sk, bytes.as_slice(), None, None).unwrap();
        use base64::Engine;
        std::fs::write(
            dir.join(SIGNATURE_FILE),
            base64::engine::general_purpose::STANDARD.encode(signature.into_string()),
        )
        .unwrap();

        // 未导入公钥：拒绝并给出导入指引。
        let error = verify_signed_release(&dir, &manifest).expect_err("未导入三方公钥应拒绝");
        assert!(format!("{error:#}").contains("未导入"), "{error:#}");

        // 导入后验签通过。
        let public_b64 = base64::engine::general_purpose::STANDARD
            .encode(keypair.pk.to_box().unwrap().into_string());
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &public_b64).unwrap();
        verify_signed_release(&dir, &manifest)
            .expect("导入公钥后三方插件应验签通过")
            .expect("应返回签名清单");

        // 移除公钥后失效。
        crate::trust::remove_trusted_publisher(root.path(), "acme-dev").unwrap();
        assert!(verify_signed_release(&dir, &manifest).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_验签通过并覆盖全树() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_ok(&dir, &manifest);
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_缺少内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |release| {
            release.content_manifest = None;
        });
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "content_manifest");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_内容清单条目路径不是固定文件名拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |release| {
            release.content_manifest.as_mut().unwrap().path = PathBuf::from("sidecar/main.mjs");
        });
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "签名制品路径不一致");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_篡改内容清单拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        std::fs::write(dir.join(CONTENT_MANIFEST_FILE), "{}").unwrap();
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "校验失败");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_篡改sidecar入口拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        std::fs::write(dir.join("sidecar/main.mjs"), "// tampered").unwrap();
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "不一致");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_篡改模板资源拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        std::fs::write(dir.join("templates/node-tool.txt"), "tampered").unwrap();
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "不一致");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_添加未列出文件拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        std::fs::write(dir.join("extra-payload.mjs"), "unknown file").unwrap();
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "未覆盖");
    }

    #[test]
    #[serial_test::serial]
    fn 三方解释器插件_签名携带sidecar二进制条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |release| {
            release.sidecar = Some(artifact(&dir, "sidecar/main.mjs"));
        });
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "二进制条目");
    }

    #[test]
    #[serial_test::serial]
    fn 官方native插件_携带内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugins").join("native-demo");
        std::fs::create_dir_all(dir.join("sidecar")).unwrap();
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"native-demo","version":"0.2.0","permissions":["sidecar.invoke"],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]},"sidecar":{"runtime":"native","binary":"sidecar/demo-bin"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html></html>").unwrap();
        std::fs::write(dir.join("sidecar/demo-bin"), b"fake native binary").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).unwrap();
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |release| {
            release.sidecar = Some(artifact(&dir, "sidecar/demo-bin"));
        });
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "content_manifest 条目");
    }

    #[test]
    #[serial_test::serial]
    fn 纯ui插件_携带内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugins").join("ui-demo");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"ui-demo","version":"0.2.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html></html>").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).unwrap();
        let key = sign_release_for_publisher(&dir, &manifest, "acme-dev", |_| {});
        crate::trust::import_trusted_publisher(root.path(), "acme-dev", &key).unwrap();
        assert_verify_err(&dir, &manifest, "仅允许解释器 sidecar 形态");
    }
}
